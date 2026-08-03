// Pollis relay-pool reconciler ("the hydra") — issue #616.
//
// Runs as a Node.js 24 Lambda on an EventBridge schedule (and on-demand).
// Each run is idempotent and converges reality to the desired-state config:
//   1. Read the desired POOL-WIDE node count from SSM Parameter Store.
//   2. Load the persisted random region placement; re-draw it if the rotation
//      interval elapsed, the pool size changed, or a region left the allowed
//      set (see placement.mjs). Persist a fresh draw back to SSM.
//   3. Set every managed region's ASG desired capacity from that placement —
//      including 0 for regions the draw skipped this rotation.
//   4. Discover each ASG's InService instances and their public IPs.
//   5. Health-check each node at GET http://<ip>:<health-port>/version and
//      keep only the ones that answer 200.
//   6. Assemble the healthy set into the signed Directory (§3 of the ticket),
//      sign the exact payload bytes with the Ed25519 private key from SSM, and
//      publish the envelope to S3 (CloudFront serves it at the stable URL).
//   7. Emit CloudWatch metrics (healthy nodes per region and pool-wide,
//      reconcile failures).
//
// Nodes move between regions without a client rebuild because the whole pool
// shares ONE pinned QUIC identity — the client pins the cert, not the address.
//
// Zero third-party deps: the AWS SDK v3 clients and node:crypto are provided by
// the Lambda Node 24 runtime. The Ed25519 signature is produced with
// crypto.sign(null, ...), i.e. pure EdDSA over the raw payload bytes — the exact
// bytes the client base64-decodes and verifies (see scripts/verify-directory.mjs
// and test/directory-contract.test.mjs, which run the client's verification path).

import { AutoScalingClient, DescribeAutoScalingGroupsCommand, UpdateAutoScalingGroupCommand, SetInstanceHealthCommand } from "@aws-sdk/client-auto-scaling";
import { EC2Client, DescribeInstancesCommand } from "@aws-sdk/client-ec2";
import { SSMClient, GetParameterCommand, PutParameterCommand } from "@aws-sdk/client-ssm";
import { S3Client, PutObjectCommand } from "@aws-sdk/client-s3";
import { CloudWatchClient, PutMetricDataCommand } from "@aws-sdk/client-cloudwatch";
import { createPrivateKey, sign } from "node:crypto";
import { readDesiredTotal, drawPlacement, placementNeedsRedraw, placementTotal, mayDrain, clamp } from "./placement.mjs";
import { parseVersion, protocolMismatch, directoryEligible, selectCycleTargets } from "./staleness.mjs";

// --- Config from the Lambda environment (set by Terraform) -------------------

const REGIONS = JSON.parse(env("MANAGED_REGIONS")); // { "us-west-2": "<asg-name>" }
const DESIRED_STATE_PARAM = env("DESIRED_STATE_PARAM"); // SSM param holding {"total": N}
const PLACEMENT_PARAM = env("PLACEMENT_PARAM"); // SSM param holding the current random draw
const SIGNING_KEY_PARAM = env("SIGNING_KEY_PARAM"); // SSM SecureString: Ed25519 private PKCS8 PEM
const IDENTITY_CERT_PARAM = env("IDENTITY_CERT_PARAM"); // SSM SecureString: base64(DER) of the pool QUIC cert
// The intended build, recorded by CI on every image roll (see relay-image.yml).
// String param holding {"image": "<immutable ref>", "sha": "<git sha>"}. The
// reconciler reads .sha to positively identify build-stale nodes; the nodes' own
// user-data reads .image to launch the intended build deterministically. Optional:
// unset on a fresh pool that has never published => nothing is cycled (staleness
// needs a positive intended sha, never an absence of one).
const INTENDED_IMAGE_PARAM = env("INTENDED_IMAGE_PARAM", "");
// The pool's expected wire identity (ALPN token, e.g. "pollis-relay/3"). A healthy
// node whose /version protocol positively DISAGREES is excluded from the signed
// directory immediately. Empty => membership is not gated on protocol.
const EXPECTED_PROTOCOL = env("EXPECTED_PROTOCOL", "");
// Bound on how many build-stale nodes to cycle per reconcile, so a roll converges
// gradually and can never empty the pool in one pass.
const MAX_CYCLE_PER_RUN = Number(env("MAX_CYCLE_PER_RUN", "1"));
const DIRECTORY_BUCKET = env("DIRECTORY_BUCKET");
const DIRECTORY_KEY = env("DIRECTORY_KEY"); // S3 object key, e.g. "directory.json"
const RELAY_PORT = Number(env("RELAY_PORT", "9444"));
const HEALTH_PORT = Number(env("HEALTH_PORT", "9445"));
const NODE_FLOOR = Number(env("NODE_FLOOR", "2"));
const NODE_MAX = Number(env("NODE_MAX", "3"));
const ROTATION_INTERVAL_SECONDS = Number(env("ROTATION_INTERVAL_HOURS", "24")) * 3600;
const DIRECTORY_TTL_SECONDS = Number(env("DIRECTORY_TTL_SECONDS", "3600"));
const HEALTH_TIMEOUT_MS = Number(env("HEALTH_TIMEOUT_MS", "2500"));
const METRIC_NAMESPACE = env("METRIC_NAMESPACE", "PollisRelayHydra");

const ssm = new SSMClient({});
const cw = new CloudWatchClient({});

export const handler = async () => {
  let reconcileFailures = 0;
  const perRegionHealthy = {};
  const relays = [];

  const placement = await resolvePlacement();
  const certB64 = await readParam(IDENTITY_CERT_PARAM, true);
  const intendedSha = await readIntendedSha();

  const unhealthyByRegion = {};
  const pendingScaleDown = {};
  // Healthy nodes that answered /version, carrying their parsed build/protocol
  // identity — the input to stale-node cycling below. A node excluded from the
  // directory on protocol grounds still lands here: it is a real, reachable node
  // and is exactly the wrong-generation instance we want to cycle out.
  const healthyNodes = [];
  let excludedFromDirectory = 0;

  for (const [region, asgName] of Object.entries(REGIONS)) {
    try {
      // 0 is a legitimate target: this region simply lost the draw this rotation.
      const target = placement[region] ?? 0;
      const group = await describeGroup(region, asgName);
      const currentDesired = group?.DesiredCapacity ?? 0;

      // Scale UP now, unconditionally — the pool can only hand over to nodes that
      // exist. Scale DOWN is deferred to after the health check (see drainLosing
      // Regions): with 3 nodes over 4 regions, roughly one rotation in ten draws
      // a placement disjoint from the current one, and zeroing the old regions in
      // the same pass would cold-start the ENTIRE pool at once. A node takes
      // minutes to boot and pull its image, so that is a real outage, on a timer.
      if (target > currentDesired) {
        await setDesiredCapacity(region, asgName, target);
      } else if (target < currentDesired) {
        pendingScaleDown[region] = { asgName, target, currentDesired };
      }

      const nodes = await instancesOf(region, group);
      const { healthy, unhealthy } = await healthCheck(nodes);
      perRegionHealthy[region] = healthy.length;
      unhealthyByRegion[region] = unhealthy;

      // Every healthy node whose PROTOCOL identity matches the pool is advertised,
      // including ones in a region on its way out — during a handover both the old
      // and new nodes serve. A healthy node whose protocol positively mismatches
      // (a wrong-generation node) is EXCLUDED from the directory here and now:
      // exclusion is instant, free and reversible, so the client never learns its
      // address and never fails ALPN against it. It is still cycled below.
      for (const node of healthy) {
        healthyNodes.push({ instanceId: node.instanceId, region, version: node.version });
        if (directoryEligible(node.version, EXPECTED_PROTOCOL)) {
          relays.push({ addr: `${node.ip}:${RELAY_PORT}`, region, cert_b64: certB64 });
        } else {
          excludedFromDirectory += 1;
          console.log(
            `${region}: excluding ${node.instanceId} from the directory — protocol ` +
              `${JSON.stringify(node.version?.protocol)} != expected ${JSON.stringify(EXPECTED_PROTOCOL)} ` +
              "(wrong-generation node; it will be cycled)",
          );
        }
      }
    } catch (err) {
      reconcileFailures += 1;
      console.error(`reconcile failed for ${region}:`, err);
    }
  }

  reconcileFailures += await drainLosingRegions(pendingScaleDown, perRegionHealthy, placement);

  // Self-heal: an instance that's InService (EC2-reachable) but whose relay
  // container is dead answers no /version, so the ASG's EC2 health check would
  // never replace it — it would linger and bill while serving nothing. Mark it
  // Unhealthy so the ASG terminates + relaunches it. ShouldRespectGracePeriod
  // shields nodes still pulling the image on first boot (see the ASG's
  // health_check_grace_period).
  //
  // Guard: only self-heal when at least one node SOMEWHERE IN THE POOL is healthy.
  // If every node everywhere is failing it's almost certainly systemic (a bad image
  // push, bad config, an SSM/identity problem) — replacing them all just churns
  // instances into the same failure and bills for it. Leave those for the alarms.
  //
  // The guard is pool-wide, not per-region, precisely because random placement
  // routinely puts a SINGLE node in a region: a per-region test would read that
  // node's dead container as "systemic" and never replace it.
  const totalHealthy = Object.values(perRegionHealthy).reduce((a, b) => a + b, 0);
  const totalUnhealthy = Object.values(unhealthyByRegion).reduce((a, n) => a + n.length, 0);
  if (totalUnhealthy > 0 && totalHealthy > 0) {
    for (const [region, unhealthy] of Object.entries(unhealthyByRegion)) {
      if (unhealthy.length > 0) {
        reconcileFailures += await markUnhealthy(region, unhealthy);
      }
    }
  } else if (totalUnhealthy > 0) {
    console.error(`all ${totalUnhealthy} node(s) across the whole pool are unhealthy — NOT self-healing (looks systemic, not per-node); leaving for the alarms`);
  }

  // Cycle build-stale nodes so the fleet converges on the intended image (#703).
  // Separate from the protocol-membership exclusion above: exclusion is instant
  // and hides a wrong-generation node from clients; cycling is the slow, capacity-
  // costing part that actually replaces the instance. Keyed on BUILD identity, so
  // a docs-only image roll (new sha, same protocol) still converges even though
  // those nodes were never a split-brain problem. Bounded per run and floor-guarded
  // so it can never empty the pool; only ever targets POSITIVELY stale nodes.
  const staleCycled = await cycleStaleNodes(healthyNodes, intendedSha, totalHealthy, placementTotal(placement));
  reconcileFailures += staleCycled.failures;

  // §3: the client REJECTS an empty relays[]. Never publish an empty directory —
  // a stale-but-valid directory that expires on its own is strictly better than
  // signing "there are no relays" (which fails every client closed immediately).
  if (relays.length === 0) {
    reconcileFailures += 1;
    console.error("no healthy relays this cycle — leaving the previous directory in place to expire on its own");
  } else {
    await publishDirectory(relays);
  }

  await emitMetrics(perRegionHealthy, reconcileFailures, {
    staleBuild: staleCycled.staleCount,
    cycled: staleCycled.cycled,
    excludedFromDirectory,
  });

  return {
    published: relays.length > 0,
    healthy: perRegionHealthy,
    placement,
    reconcileFailures,
    intendedSha,
    excludedFromDirectory,
    staleBuild: staleCycled.staleCount,
    cycled: staleCycled.cycled,
  };
};

// --- Intended build (recorded by CI on every image roll) ---------------------

// Read the intended build's git sha from the CI-owned SSM parameter. Returns null
// when the parameter is unset (fresh pool that never published), unreadable, or
// malformed — a null intended sha positively identifies NOTHING as stale, so the
// pool is left exactly as it is rather than cycled on a guess. A read failure is
// logged and swallowed on purpose: the intended-image record is auxiliary to the
// core reconcile (placement, health, directory), and wedging the whole cycle on it
// would take the directory down with it.
async function readIntendedSha() {
  if (!INTENDED_IMAGE_PARAM) {
    return null;
  }
  let raw;
  try {
    raw = await readParam(INTENDED_IMAGE_PARAM, false);
  } catch (err) {
    if (err?.name === "ParameterNotFound") {
      console.log("no intended-image parameter yet — not cycling any node on build identity");
    } else {
      console.error("failed to read the intended-image parameter — not cycling this run:", err);
    }
    return null;
  }
  try {
    const parsed = JSON.parse(raw);
    const sha = typeof parsed?.sha === "string" ? parsed.sha : null;
    if (!sha) {
      console.error(`intended-image parameter has no string sha: ${raw}`);
    }
    return sha;
  } catch (err) {
    console.error(`intended-image parameter is not valid JSON, ignoring it: ${err?.message ?? err}`);
    return null;
  }
}

// Pick and terminate build-stale nodes at a bounded, floor-guarded rate. Marking a
// node Unhealthy makes its ASG replace it (SetInstanceHealth — the same seam the
// self-heal uses); the replacement's user-data reads the intended-image param and
// boots the intended build. Returns { staleCount, cycled, failures }.
async function cycleStaleNodes(healthyNodes, intendedSha, totalHealthy, poolTotal) {
  const { targets, buildStale } = selectCycleTargets({
    nodes: healthyNodes,
    intendedSha,
    healthyTotal: totalHealthy,
    poolTotal,
    nodeFloor: NODE_FLOOR,
    maxPerRun: MAX_CYCLE_PER_RUN,
  });

  if (buildStale.length === 0) {
    return { staleCount: 0, cycled: 0, failures: 0 };
  }

  if (targets.length === 0) {
    console.log(
      `${buildStale.length} build-stale node(s) found but deferring: floor guard leaves no cycling budget ` +
        `this run (healthy ${totalHealthy}, floor ${NODE_FLOOR}). A later reconcile will cycle them once ` +
        "replacement capacity is healthy.",
    );
    return { staleCount: buildStale.length, cycled: 0, failures: 0 };
  }

  // Group targets by region — SetInstanceHealth is issued against the region's ASG.
  const byRegion = {};
  for (const t of targets) {
    (byRegion[t.region] ??= []).push({ instanceId: t.instanceId });
  }

  let failures = 0;
  for (const [region, nodes] of Object.entries(byRegion)) {
    for (const { instanceId } of nodes) {
      const target = targets.find((t) => t.instanceId === instanceId);
      console.log(
        `${region}: cycling build-stale ${instanceId} — sha ${JSON.stringify(target?.version?.sha)} != ` +
          `intended ${JSON.stringify(intendedSha)} (marking Unhealthy; ASG will relaunch on the intended build)`,
      );
    }
    failures += await markUnhealthy(region, nodes, "build-stale — cycling to the intended image");
  }
  // markUnhealthy returns the count it FAILED to flag; the rest were cycled.
  const cycled = Math.max(0, targets.length - failures);
  return { staleCount: buildStale.length, cycled, failures };
}

// --- Random region placement (see placement.mjs) -----------------------------

// Load the persisted draw and decide whether it still holds; re-draw + persist if
// not. Returns a region -> node-count map covering this rotation.
async function resolvePlacement() {
  const allowedRegions = Object.keys(REGIONS);
  const total = clamp(await readDesiredTotalFromSsm(), NODE_FLOOR, NODE_MAX);
  const now = Math.floor(Date.now() / 1000);

  // Only a genuinely ABSENT parameter means "never drawn". Any other failure —
  // throttling, a 5xx, KMS — must abort the reconcile, because treating it as
  // "no placement recorded yet" would re-randomize the whole pool on a transient
  // blip. Unparseable JSON is left to the redraw predicate, which reports it as
  // malformed and draws over it (that one IS recoverable, and wedging on it would
  // need a human).
  let current = null;
  try {
    current = JSON.parse(await readParam(PLACEMENT_PARAM, false));
  } catch (err) {
    if (err?.name === "ParameterNotFound") {
      console.log("no placement parameter yet — first draw");
    } else if (err instanceof SyntaxError) {
      console.error("placement parameter is not valid JSON, will re-draw over it:", err);
    } else {
      throw new Error(`failed to read the placement parameter: ${err?.message ?? err}`, { cause: err });
    }
  }

  const reason = placementNeedsRedraw(current, {
    allowedRegions,
    total,
    now,
    rotationIntervalSeconds: ROTATION_INTERVAL_SECONDS,
  });
  if (!reason) {
    return current.placement;
  }

  const placement = drawPlacement(allowedRegions, total, Math.random);
  console.log(`rotating placement (${reason}): ${JSON.stringify(placement)}`);

  // Persist BEFORE acting on it. If the write fails we throw rather than scale:
  // scaling to an unrecorded draw would re-randomize on the very next reconcile
  // and churn the pool every two minutes.
  await writeParam(PLACEMENT_PARAM, JSON.stringify({ drawn_at: now, placement }));
  return placement;
}

// Deliberately does NOT catch. Falling back to the floor on a read failure looks
// identical to an operator having scaled the pool down, so the rotation predicate
// would re-draw and the pool would shrink — then grow and re-draw again on the
// next successful read. One transient SSM error would cost two full unscheduled
// rotations. Throwing leaves the pool untouched and trips the Errors alarm.
async function readDesiredTotalFromSsm() {
  return readDesiredTotal(await readParam(DESIRED_STATE_PARAM, false));
}

// --- Directory assembly + signing (§3 frozen contract) -----------------------

async function publishDirectory(relays) {
  // issued_at/expires_at are unix seconds. Kept short (default +1h) and re-signed
  // every reconcile so a rolled-back/stale directory expires quickly.
  const issuedAt = Math.floor(Date.now() / 1000);
  const directory = {
    version: 1,
    issued_at: issuedAt,
    expires_at: issuedAt + DIRECTORY_TTL_SECONDS,
    relays,
  };

  // Sign-then-encode LITERAL bytes: we sign the exact UTF-8 bytes we base64 into
  // payload_b64. No canonicalization — the client verifies over the exact bytes
  // it base64-decodes, so both sides are byte-for-byte identical.
  const payloadBytes = Buffer.from(JSON.stringify(directory), "utf8");
  const privatePem = await readParam(SIGNING_KEY_PARAM, true);
  const privateKey = createPrivateKey({ key: privatePem, format: "pem" });
  const signature = sign(null, payloadBytes, privateKey); // Ed25519 => digest algo is null

  const envelope = {
    payload_b64: payloadBytes.toString("base64"),
    signature_b64: signature.toString("base64"),
  };

  const s3 = new S3Client({}); // bucket region resolved by the SDK
  await s3.send(new PutObjectCommand({
    Bucket: DIRECTORY_BUCKET,
    Key: DIRECTORY_KEY,
    Body: JSON.stringify(envelope),
    ContentType: "application/json",
    // Short TTL so a re-sign propagates quickly through CloudFront.
    CacheControl: "public, max-age=30",
  }));

  console.log(`published directory: ${relays.length} relay(s), expires_at=${directory.expires_at}`);
}

// --- ASG reconcile -----------------------------------------------------------

// Drain the regions the draw moved away from — but only once the pool can stand
// without them, so a rotation is a handover rather than a cutover.
//
// Deferring is safe precisely because every reconcile is idempotent: the next
// tick re-reads the same persisted draw, finds the same regions over capacity,
// and drains them as soon as the incoming nodes answer /version. The cost of
// waiting is a few minutes of running both sets; the cost of not waiting is the
// whole pool cold-starting at once. Nothing here can strand capacity forever —
// if the new nodes never come up, the old ones keep serving, which is the
// failure mode we want.
async function drainLosingRegions(pending, perRegionHealthy, placement) {
  const draining = Object.keys(pending);
  if (draining.length === 0) {
    return 0;
  }

  const { ok, retainedHealthy, required } = mayDrain({
    draining,
    perRegionHealthy,
    poolTotal: placementTotal(placement),
    nodeFloor: NODE_FLOOR,
  });

  if (!ok) {
    console.log(
      `deferring scale-down of ${draining.join(", ")}: ${retainedHealthy} healthy node(s) outside them, need ${required}. ` +
        "A later reconcile will drain them once the incoming nodes answer /version.",
    );
    return 0;
  }

  let failures = 0;
  for (const [region, { asgName, target, currentDesired }] of Object.entries(pending)) {
    try {
      await setDesiredCapacity(region, asgName, target);
      console.log(`${region}: drained ${currentDesired} -> ${target} (${retainedHealthy} healthy elsewhere)`);
    } catch (err) {
      failures += 1;
      console.error(`${region}: failed to scale down to ${target}:`, err);
    }
  }
  return failures;
}

async function setDesiredCapacity(region, asgName, target) {
  const asg = new AutoScalingClient({ region });
  await asg.send(new UpdateAutoScalingGroupCommand({
    AutoScalingGroupName: asgName,
    DesiredCapacity: target,
  }));
  console.log(`${region}: set ASG ${asgName} desired capacity -> ${target}`);
}

// Split out of the old discoverInServiceNodes so the group is described ONCE per
// region: the scale-up/scale-down decision needs the current DesiredCapacity, and
// re-describing to get it would double the API calls and let the two reads
// disagree mid-reconcile.
async function describeGroup(region, asgName) {
  const asg = new AutoScalingClient({ region });
  const groups = await asg.send(new DescribeAutoScalingGroupsCommand({
    AutoScalingGroupNames: [asgName],
  }));
  return groups.AutoScalingGroups?.[0];
}

async function instancesOf(region, group) {
  if (!group) {
    return [];
  }
  const ec2 = new EC2Client({ region });

  const instanceIds = (group.Instances ?? [])
    .filter((i) => i.LifecycleState === "InService")
    .map((i) => i.InstanceId);
  if (instanceIds.length === 0) {
    return [];
  }

  const desc = await ec2.send(new DescribeInstancesCommand({ InstanceIds: instanceIds }));
  const nodes = [];
  for (const reservation of desc.Reservations ?? []) {
    for (const inst of reservation.Instances ?? []) {
      // A node with no public IP yet is still wiring up its ENI — skip it this
      // cycle rather than treat it as a dead relay (it isn't advertised, and the
      // grace period covers it if it's genuinely mid-boot).
      if (inst.PublicIpAddress) {
        nodes.push({ instanceId: inst.InstanceId, ip: inst.PublicIpAddress });
      }
    }
  }
  return nodes;
}

// Flag failed nodes as Unhealthy so the ASG replaces them. Scoped to the app=
// pollis-relay ASGs by the reconciler's IAM policy. ShouldRespectGracePeriod keeps
// a freshly-launched node (still pulling the image) from being killed mid-boot.
// Returns the number of instances it FAILED to flag. Self-heal that silently
// stops working is indistinguishable from a pool with nothing wrong: if the IAM
// condition stops matching or the call is throttled, dead nodes linger, bill, and
// serve nothing, with no metric moving. Counting the failures puts them on
// ReconcileFailures, which is alarmed.
async function markUnhealthy(region, unhealthyNodes, reason = "no /version") {
  const asg = new AutoScalingClient({ region });
  let failures = 0;
  for (const { instanceId } of unhealthyNodes) {
    try {
      await asg.send(new SetInstanceHealthCommand({
        InstanceId: instanceId,
        HealthStatus: "Unhealthy",
        ShouldRespectGracePeriod: true,
      }));
      console.log(`${region}: marked ${instanceId} Unhealthy (${reason}) — ASG will replace it`);
    } catch (err) {
      failures += 1;
      console.error(`${region}: failed to mark ${instanceId} unhealthy:`, err);
    }
  }
  return failures;
}

// --- Health check ------------------------------------------------------------

async function healthCheck(nodes) {
  const results = await Promise.all(nodes.map(async (node) => {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), HEALTH_TIMEOUT_MS);
    try {
      // Treat any 200 as healthy AND parse the /version body: its sha/protocol are
      // what directory membership and stale-node cycling turn on (#703). A 200 with
      // an unreadable body is still healthy — parseVersion returns null and every
      // staleness predicate fails toward "keep the node", so a garbled body can
      // never get a reachable node cycled or excluded.
      const res = await fetch(`http://${node.ip}:${HEALTH_PORT}/version`, { signal: controller.signal });
      if (!res.ok) {
        return { node: { ...node, version: null }, ok: false };
      }
      let version = null;
      try {
        version = parseVersion(await res.text());
      } catch {
        version = null;
      }
      return { node: { ...node, version }, ok: true };
    } catch {
      return { node: { ...node, version: null }, ok: false };
    } finally {
      clearTimeout(timer);
    }
  }));
  return {
    healthy: results.filter((r) => r.ok).map((r) => r.node),
    unhealthy: results.filter((r) => !r.ok).map((r) => r.node),
  };
}

// --- Metrics -----------------------------------------------------------------

async function emitMetrics(perRegionHealthy, reconcileFailures, stale = {}) {
  const timestamp = new Date();
  const data = [
    { MetricName: "ReconcileFailures", Value: reconcileFailures, Unit: "Count", Timestamp: timestamp },
    // Split-brain visibility (#703): how many healthy nodes are on the wrong build,
    // how many were cycled this run, and how many were hidden from the directory on
    // a protocol mismatch. StaleBuildNodes trending non-zero for long means the roll
    // isn't converging (e.g. cycling deferred by the floor guard every run).
    { MetricName: "StaleBuildNodes", Value: stale.staleBuild ?? 0, Unit: "Count", Timestamp: timestamp },
    { MetricName: "NodesCycled", Value: stale.cycled ?? 0, Unit: "Count", Timestamp: timestamp },
    { MetricName: "ExcludedFromDirectory", Value: stale.excludedFromDirectory ?? 0, Unit: "Count", Timestamp: timestamp },
    // Pool-wide healthy count. This is what the floor alarm watches: with random
    // placement an individual region legitimately holds zero nodes, so a
    // per-region floor alarm would page on every rotation that skipped it.
    {
      MetricName: "HealthyNodesTotal",
      Value: Object.values(perRegionHealthy).reduce((a, b) => a + b, 0),
      Unit: "Count",
      Timestamp: timestamp,
    },
  ];
  // Still emitted per region, for visibility into where the draw put things — but
  // deliberately un-alarmed.
  for (const [region, count] of Object.entries(perRegionHealthy)) {
    data.push({
      MetricName: "HealthyNodes",
      Value: count,
      Unit: "Count",
      Timestamp: timestamp,
      Dimensions: [{ Name: "Region", Value: region }],
    });
  }
  try {
    await cw.send(new PutMetricDataCommand({ Namespace: METRIC_NAMESPACE, MetricData: data }));
  } catch (err) {
    console.error("failed to emit metrics:", err);
  }
}

// --- SSM helpers -------------------------------------------------------------

async function readParam(name, decrypt) {
  const out = await ssm.send(new GetParameterCommand({ Name: name, WithDecryption: decrypt }));
  return out.Parameter.Value;
}

async function writeParam(name, value) {
  await ssm.send(new PutParameterCommand({ Name: name, Value: value, Type: "String", Overwrite: true }));
}

// --- utils -------------------------------------------------------------------

function env(name, fallback) {
  const v = process.env[name];
  if (v === undefined || v === "") {
    if (fallback !== undefined) {
      return fallback;
    }
    throw new Error(`missing required env var ${name}`);
  }
  return v;
}
