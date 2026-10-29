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
//   6. Read the operator-authored REVOCATION set, exclude every revoked node
//      from the directory, and destroy those nodes (#813).
//   6b. Aggregate PEER relays parked with surviving nodes, minus revoked ones (#813 wave 3).
//   7. Sign and publish the Directory to S3, after the short-lived revocations.json it anchors.
//   8. Emit CloudWatch metrics (healthy nodes per region and pool-wide,
//      reconcile failures, revocation counts).
//
// Nodes move between regions without a client rebuild because the whole pool
// shares ONE pinned QUIC identity — the client pins the cert, not the address.
//
// Zero third-party deps (AWS SDK v3 ships in the Lambda runtime); Ed25519 over the raw payload bytes.

import { AutoScalingClient, DescribeAutoScalingGroupsCommand, UpdateAutoScalingGroupCommand, SetInstanceHealthCommand } from "@aws-sdk/client-auto-scaling";
import { EC2Client, DescribeInstancesCommand } from "@aws-sdk/client-ec2";
import { SSMClient, GetParameterCommand, PutParameterCommand } from "@aws-sdk/client-ssm";
import { S3Client, PutObjectCommand } from "@aws-sdk/client-s3";
import { CloudWatchClient, PutMetricDataCommand } from "@aws-sdk/client-cloudwatch";
import { createPrivateKey, sign } from "node:crypto";
import { readDesiredTotal, drawPlacement, placementNeedsRedraw, placementTotal, mayDrain, clamp } from "./placement.mjs";
import { parseVersion, protocolMismatch, directoryEligible, selectCycleTargets } from "./staleness.mjs";
import { parseRevocationSet, revokesRelay, buildRevocationPayload, directoryAnchor, directoryTtlFor } from "./revocation.mjs";
import { PEERS_PATH, parsePeerReport, aggregatePeers, excludeRevokedPeers } from "./peers.mjs";

const REGIONS = JSON.parse(env("MANAGED_REGIONS")); // { "us-west-2": "<asg-name>" }
const DESIRED_STATE_PARAM = env("DESIRED_STATE_PARAM"); // SSM param holding {"total": N}
const PLACEMENT_PARAM = env("PLACEMENT_PARAM"); // SSM param holding the current random draw
const SIGNING_KEY_PARAM = env("SIGNING_KEY_PARAM"); // SSM SecureString: Ed25519 private PKCS8 PEM
const IDENTITY_CERT_PARAM = env("IDENTITY_CERT_PARAM"); // SSM SecureString: base64(DER) of the pool QUIC cert
// CI's record of the intended build ({"image","sha"}); unset means nothing is cycled.
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
// Revocation (#813): {"revoked":[...]}; its SSM VERSION is the seq. Unset means not configured (pre-#813 shape).
const REVOCATIONS_PARAM = env("REVOCATIONS_PARAM", "");
const REVOCATIONS_KEY = env("REVOCATIONS_KEY", "revocations.json"); // S3 object key / URL path
const RELAY_PORT = Number(env("RELAY_PORT", "9444"));
const HEALTH_PORT = Number(env("HEALTH_PORT", "9445"));
const NODE_FLOOR = Number(env("NODE_FLOOR", "2"));
const NODE_MAX = Number(env("NODE_MAX", "3"));
const ROTATION_INTERVAL_SECONDS = Number(env("ROTATION_INTERVAL_HOURS", "24")) * 3600;
const DIRECTORY_TTL_SECONDS = Number(env("DIRECTORY_TTL_SECONDS", "3600"));
// The revocation list's own, much shorter life — this is the real exposure window
// for a seized relay, and the whole point of publishing it separately.
const REVOCATION_TTL_SECONDS = Number(env("REVOCATION_TTL_SECONDS", "300"));
// Directory TTL used WHILE any relay is revoked; see directoryTtlFor().
const REVOKED_DIRECTORY_TTL_SECONDS = Number(env("REVOKED_DIRECTORY_TTL_SECONDS", "300"));
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
  const revocations = await readRevocations();

  const unhealthyByRegion = {};
  // Revoked healthy nodes: excluded from the directory and destroyed below.
  const revokedNodes = [];
  const pendingScaleDown = {};
  // Healthy nodes with parsed /version identity, the input to stale cycling (protocol-excluded ones included).
  const healthyNodes = [];
  // Nodes that made it into `relays` — the set polled for parked peers (#813).
  const directoryNodes = [];
  let excludedFromDirectory = 0;

  for (const [region, asgName] of Object.entries(REGIONS)) {
    try {
      // 0 is a legitimate target: this region simply lost the draw this rotation.
      const target = placement[region] ?? 0;
      const group = await describeGroup(region, asgName);
      const currentDesired = group?.DesiredCapacity ?? 0;

      // Scale UP now; scale DOWN waits for the health check (drainLosingRegions) so a disjoint draw cannot cold-start the pool.
      if (target > currentDesired) {
        await setDesiredCapacity(region, asgName, target);
      } else if (target < currentDesired) {
        pendingScaleDown[region] = { asgName, target, currentDesired };
      }

      const nodes = await instancesOf(region, group);
      const { healthy, unhealthy } = await healthCheck(nodes);
      perRegionHealthy[region] = healthy.length;
      unhealthyByRegion[region] = unhealthy;

      // Advertise healthy nodes whose protocol matches (old regions too, mid-handover); mismatches are excluded now, cycled later.
      for (const node of healthy) {
        healthyNodes.push({ instanceId: node.instanceId, region, version: node.version });
        const candidate = { addr: `${node.ip}:${RELAY_PORT}`, region, cert_b64: certB64 };
        // Revocation outranks every other membership rule: a revoked node never
        // enters the directory, whatever its build, protocol or health says.
        if (revocations.ok && revokesRelay(revocations.revoked, candidate)) {
          revokedNodes.push({ instanceId: node.instanceId, region, addr: candidate.addr });
          console.log(`${region}: ${node.instanceId} (${candidate.addr}) is REVOKED — excluded from the directory and being destroyed`);
          continue;
        }
        if (directoryEligible(node.version, EXPECTED_PROTOCOL)) {
          relays.push(candidate);
          // Only advertised nodes are asked for parked peers (`parked_at` must name a dialable relay).
          directoryNodes.push({ ip: node.ip, addr: candidate.addr });
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

  // Self-heal: mark InService nodes with a dead relay Unhealthy so the ASG replaces them (grace period respected).
  // Only when some node in the POOL is healthy (all failing = systemic); pool-wide since a region may hold one node.
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

  // Cycle build-stale nodes onto the intended image (#703): bounded, floor-guarded, positive identification only.
  const staleCycled = await cycleStaleNodes(healthyNodes, intendedSha, totalHealthy, placementTotal(placement));
  reconcileFailures += staleCycled.failures;

  // Destroy revoked nodes: deliberately unbounded and not floor-guarded (untrusted, not merely stale).
  reconcileFailures += await destroyRevokedNodes(revokedNodes);

  // Peer relays (#813 wave 3), filtered by the same revocation set; this can never fail the cycle.
  const collectedPeers = await collectPeers(directoryNodes);
  const { peers, excluded: revokedPeers } = revocations.ok
    ? excludeRevokedPeers(collectedPeers, revocations.revoked)
    : { peers: [], excluded: 0 };
  if (revokedPeers > 0) {
    console.log(`excluded ${revokedPeers} REVOKED peer relay(s) from the directory`);
  }

  // Order is load-bearing: the revocation list FIRST, then the directory that anchors it.
  let published = false;
  if (revocations.configured && !revocations.ok) {
    // Revocation state unknown: publish nothing; the list expires within REVOCATION_TTL_SECONDS (fail closed).
    reconcileFailures += 1;
    console.error(`revocation set unusable (${revocations.error}) — publishing NOTHING this cycle; the live artifacts will expire on their own`);
  } else if (relays.length === 0) {
    // §3: the client REJECTS an empty relays[]. Never publish an empty directory —
    // a stale-but-valid directory that expires on its own is strictly better than
    // signing "there are no relays" (which fails every client closed immediately).
    reconcileFailures += 1;
    console.error("no healthy relays this cycle — leaving the previous directory in place to expire on its own");
  } else {
    if (revocations.configured) {
      await publishRevocations(revocations);
    }
    await publishDirectory(relays, revocations, peers);
    published = true;
  }

  await emitMetrics(perRegionHealthy, reconcileFailures, {
    staleBuild: staleCycled.staleCount,
    cycled: staleCycled.cycled,
    excludedFromDirectory,
    revokedRelays: revocations.ok ? revocations.revoked.length : 0,
    revokedNodesDestroyed: revokedNodes.length,
    peerRelays: peers.length,
    revokedPeers,
  });

  return {
    published,
    healthy: perRegionHealthy,
    placement,
    reconcileFailures,
    intendedSha,
    excludedFromDirectory,
    staleBuild: staleCycled.staleCount,
    cycled: staleCycled.cycled,
    revocation: {
      configured: revocations.configured,
      ok: revocations.ok,
      seq: revocations.seq,
      count: revocations.revoked.length,
      nodesDestroyed: revokedNodes.length,
    },
    peers: { count: peers.length, revoked: revokedPeers },
  };
};

// Ask every node we are advertising which peer relays are parked with it, and
// aggregate the answers into the directory's additive `peers` array.
// Every failure (404 from older relays, timeout, garbage) yields fewer peers, never a failed cycle.
async function collectPeers(nodes) {
  if (nodes.length === 0) {
    return [];
  }
  const reports = await Promise.all(nodes.map(async (node) => {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), HEALTH_TIMEOUT_MS);
    try {
      const res = await fetch(`http://${node.ip}:${HEALTH_PORT}${PEERS_PATH}`, { signal: controller.signal });
      if (!res.ok) {
        return { addr: node.addr, peers: [] };
      }
      return { addr: node.addr, peers: parsePeerReport(await res.text()) };
    } catch {
      return { addr: node.addr, peers: [] };
    } finally {
      clearTimeout(timer);
    }
  }));
  return aggregatePeers(reports);
}

// Read + validate the operator-authored revocation set, and take its SSM
// parameter VERSION as the published sequence number (see revocation.mjs for why
// that is the counter rather than an operator-authored field).
//
// Outcomes: NOT CONFIGURED (pre-#813 shape), OK, or NOT OK (unknown: the caller publishes nothing).
async function readRevocations() {
  if (!REVOCATIONS_PARAM) {
    return { configured: false, ok: true, seq: null, revoked: [] };
  }
  let param;
  try {
    param = await readParamWithVersion(REVOCATIONS_PARAM);
  } catch (err) {
    return {
      configured: true,
      ok: false,
      seq: null,
      revoked: [],
      error: `failed to read ${REVOCATIONS_PARAM}: ${err?.message ?? err}`,
    };
  }
  try {
    const { revoked } = parseRevocationSet(param.value, RELAY_PORT);
    if (revoked.length > 0) {
      console.log(`revocation set seq=${param.version}: ${revoked.length} revoked relay(s)`);
    }
    return { configured: true, ok: true, seq: param.version, revoked };
  } catch (err) {
    return {
      configured: true,
      ok: false,
      seq: param.version,
      revoked: [],
      error: `malformed revocation set: ${err?.message ?? err}`,
    };
  }
}

// Mark revoked nodes Unhealthy; returns how many it FAILED to flag (alarmed via ReconcileFailures).
async function destroyRevokedNodes(revokedNodes) {
  if (revokedNodes.length === 0) {
    return 0;
  }
  const byRegion = {};
  for (const node of revokedNodes) {
    (byRegion[node.region] ??= []).push({ instanceId: node.instanceId });
  }
  let failures = 0;
  for (const [region, nodes] of Object.entries(byRegion)) {
    // ShouldRespectGracePeriod is FALSE here (markUnhealthy's default is true):
    // a revoked node must go now, not after its boot grace period.
    failures += await markUnhealthy(region, nodes, "REVOKED", false);
  }
  return failures;
}

// Sign + publish revocations.json, even when empty: clients need a FRESH "nothing revoked".
async function publishRevocations(revocations) {
  const issuedAt = Math.floor(Date.now() / 1000);
  const payload = buildRevocationPayload({
    seq: revocations.seq,
    revoked: revocations.revoked,
    issuedAt,
    ttlSeconds: REVOCATION_TTL_SECONDS,
  });
  const envelope = await signEnvelope(payload);

  const s3 = new S3Client({});
  await s3.send(new PutObjectCommand({
    Bucket: DIRECTORY_BUCKET,
    Key: REVOCATIONS_KEY,
    Body: JSON.stringify(envelope),
    ContentType: "application/json",
    // Shorter than the directory's 30s: this is the artifact whose staleness is
    // a security property, so edge caching gets less room.
    CacheControl: "public, max-age=10, must-revalidate",
  }));

  console.log(`published revocations: seq=${payload.seq} ${payload.revoked.length} entr(ies), expires_at=${payload.expires_at}`);
}

// The intended build's sha from CI's SSM param, or null (cycles nothing); read errors are logged, not fatal.
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

// Mark build-stale nodes Unhealthy at a bounded, floor-guarded rate; the ASG relaunches the intended image.
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

// Load the persisted draw and decide whether it still holds; re-draw + persist if
// not. Returns a region -> node-count map covering this rotation.
async function resolvePlacement() {
  const allowedRegions = Object.keys(REGIONS);
  const total = clamp(await readDesiredTotalFromSsm(), NODE_FLOOR, NODE_MAX);
  const now = Math.floor(Date.now() / 1000);

  // Only an ABSENT parameter means "never drawn"; other read errors abort (else a blip re-randomizes the pool).
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

// Deliberately does NOT catch: a fallback total would look like a resize and trigger unscheduled rotations.
async function readDesiredTotalFromSsm() {
  return readDesiredTotal(await readParam(DESIRED_STATE_PARAM, false));
}

async function publishDirectory(relays, revocations, peers = []) {
  // issued_at/expires_at are unix seconds. Kept short (default +1h) and re-signed
  // every reconcile so a rolled-back/stale directory expires quickly — and cut
  // further while anything is revoked (see directoryTtlFor).
  const issuedAt = Math.floor(Date.now() / 1000);
  const ttl = directoryTtlFor({
    revokedCount: revocations.revoked.length,
    baseTtl: DIRECTORY_TTL_SECONDS,
    activeTtl: REVOKED_DIRECTORY_TTL_SECONDS,
  });
  const directory = {
    version: 1,
    issued_at: issuedAt,
    expires_at: issuedAt + ttl,
    relays,
  };

  // ADDITIVE, OPTIONAL field: `version` stays 1 so shipped clients still verify; omitted when not configured.
  const anchor = revocations.configured
    ? directoryAnchor({ seq: revocations.seq, path: REVOCATIONS_KEY, count: revocations.revoked.length })
    : null;
  if (anchor !== null) {
    directory.revocation = anchor;
  }

  // Second additive field (#813 wave 3), same terms; omitted when empty (absent means "no peers").
  if (peers.length > 0) {
    directory.peers = peers;
  }

  // Sign-then-encode LITERAL bytes: we sign the exact UTF-8 bytes we base64 into
  // payload_b64. No canonicalization — the client verifies over the exact bytes
  // it base64-decodes, so both sides are byte-for-byte identical.
  const envelope = await signEnvelope(directory);

  const s3 = new S3Client({}); // bucket region resolved by the SDK
  await s3.send(new PutObjectCommand({
    Bucket: DIRECTORY_BUCKET,
    Key: DIRECTORY_KEY,
    Body: JSON.stringify(envelope),
    ContentType: "application/json",
    // Short TTL so a re-sign propagates quickly through CloudFront.
    CacheControl: "public, max-age=30",
  }));

  console.log(`published directory: ${relays.length} relay(s), ${peers.length} peer relay(s), expires_at=${directory.expires_at}${anchor ? ` revocation.seq=${anchor.seq}` : ""}`);
}

// Sign an object into the §3 envelope shape. Shared by both signed artifacts so
// the directory and the revocation list can never drift in how they are signed.
async function signEnvelope(payload) {
  const payloadBytes = Buffer.from(JSON.stringify(payload), "utf8");
  const privatePem = await readParam(SIGNING_KEY_PARAM, true);
  const privateKey = createPrivateKey({ key: privatePem, format: "pem" });
  const signature = sign(null, payloadBytes, privateKey); // Ed25519 => digest algo is null
  return {
    payload_b64: payloadBytes.toString("base64"),
    signature_b64: signature.toString("base64"),
  };
}

// Drain the regions the draw moved away from — but only once the pool can stand
// without them, so a rotation is a handover rather than a cutover.
// Deferring is safe: every reconcile is idempotent, so the next tick drains once new nodes answer.
async function drainLosingRegions(pending, perRegionHealthy, placement) {
  const draining = Object.keys(pending);
  if (draining.length === 0) {
    return 0;
  }

  // Pass POST-DRAIN TARGETS: a 2 -> 1 region keeps a node, and counting it as lost deferred forever.
  const { ok, retainedHealthy, required } = mayDrain({
    draining: Object.fromEntries(Object.entries(pending).map(([region, { target }]) => [region, target])),
    perRegionHealthy,
    poolTotal: placementTotal(placement),
    nodeFloor: NODE_FLOOR,
  });

  if (!ok) {
    console.log(
      `deferring scale-down of ${draining.join(", ")}: ${retainedHealthy} healthy node(s) would survive it, need ${required}. ` +
        "A later reconcile will drain them once the incoming nodes answer /version.",
    );
    return 0;
  }

  let failures = 0;
  for (const [region, { asgName, target, currentDesired }] of Object.entries(pending)) {
    try {
      await setDesiredCapacity(region, asgName, target);
      console.log(`${region}: drained ${currentDesired} -> ${target} (${retainedHealthy} healthy node(s) survive the drain)`);
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

// Described once per region so scaling and discovery see the same DesiredCapacity.
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

// Flag failed nodes Unhealthy (grace period protects mid-boot nodes); returns failures, alarmed via ReconcileFailures.
async function markUnhealthy(region, unhealthyNodes, reason = "no /version", respectGracePeriod = true) {
  const asg = new AutoScalingClient({ region });
  let failures = 0;
  for (const { instanceId } of unhealthyNodes) {
    try {
      await asg.send(new SetInstanceHealthCommand({
        InstanceId: instanceId,
        HealthStatus: "Unhealthy",
        ShouldRespectGracePeriod: respectGracePeriod,
      }));
      console.log(`${region}: marked ${instanceId} Unhealthy (${reason}) — ASG will replace it`);
    } catch (err) {
      failures += 1;
      console.error(`${region}: failed to mark ${instanceId} unhealthy:`, err);
    }
  }
  return failures;
}

async function healthCheck(nodes) {
  const results = await Promise.all(nodes.map(async (node) => {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), HEALTH_TIMEOUT_MS);
    try {
      // Any 200 is healthy; the parsed /version (or null) drives membership and cycling (#703).
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
    // Revocation (#813): sustained RevokedNodesDestroyed means the ASG keeps re-issuing a revoked address.
    { MetricName: "RevokedRelays", Value: stale.revokedRelays ?? 0, Unit: "Count", Timestamp: timestamp },
    { MetricName: "RevokedNodesDestroyed", Value: stale.revokedNodesDestroyed ?? 0, Unit: "Count", Timestamp: timestamp },
    // Peers (#813 wave 3): PeerRelays dropping to zero means parking is broken (paths silently shorten).
    { MetricName: "PeerRelays", Value: stale.peerRelays ?? 0, Unit: "Count", Timestamp: timestamp },
    { MetricName: "RevokedPeers", Value: stale.revokedPeers ?? 0, Unit: "Count", Timestamp: timestamp },
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

async function readParam(name, decrypt) {
  const out = await ssm.send(new GetParameterCommand({ Name: name, WithDecryption: decrypt }));
  return out.Parameter.Value;
}

// Read a parameter with its SSM Version (the revocation seq).
async function readParamWithVersion(name) {
  const out = await ssm.send(new GetParameterCommand({ Name: name, WithDecryption: false }));
  const version = out.Parameter?.Version;
  if (!Number.isInteger(version) || version < 0) {
    throw new Error(`parameter ${name} returned no usable Version (${version})`);
  }
  return { value: out.Parameter.Value, version };
}

async function writeParam(name, value) {
  await ssm.send(new PutParameterCommand({ Name: name, Value: value, Type: "String", Overwrite: true }));
}

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
