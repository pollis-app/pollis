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
import { readDesiredTotal, drawPlacement, placementNeedsRedraw, placementTotal, clamp } from "./placement.mjs";

// --- Config from the Lambda environment (set by Terraform) -------------------

const REGIONS = JSON.parse(env("MANAGED_REGIONS")); // { "us-west-2": "<asg-name>" }
const DESIRED_STATE_PARAM = env("DESIRED_STATE_PARAM"); // SSM param holding {"total": N}
const PLACEMENT_PARAM = env("PLACEMENT_PARAM"); // SSM param holding the current random draw
const SIGNING_KEY_PARAM = env("SIGNING_KEY_PARAM"); // SSM SecureString: Ed25519 private PKCS8 PEM
const IDENTITY_CERT_PARAM = env("IDENTITY_CERT_PARAM"); // SSM SecureString: base64(DER) of the pool QUIC cert
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

  const unhealthyByRegion = {};

  for (const [region, asgName] of Object.entries(REGIONS)) {
    try {
      // 0 is a legitimate target: this region simply lost the draw this rotation.
      const target = placement[region] ?? 0;
      await setDesiredCapacity(region, asgName, target);

      const nodes = await discoverInServiceNodes(region, asgName);
      const { healthy, unhealthy } = await healthCheck(nodes);
      perRegionHealthy[region] = healthy.length;
      unhealthyByRegion[region] = unhealthy;

      for (const { ip } of healthy) {
        relays.push({ addr: `${ip}:${RELAY_PORT}`, region, cert_b64: certB64 });
      }
    } catch (err) {
      reconcileFailures += 1;
      console.error(`reconcile failed for ${region}:`, err);
    }
  }

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
        await markUnhealthy(region, unhealthy);
      }
    }
  } else if (totalUnhealthy > 0) {
    console.error(`all ${totalUnhealthy} node(s) across the whole pool are unhealthy — NOT self-healing (looks systemic, not per-node); leaving for the alarms`);
  }

  // §3: the client REJECTS an empty relays[]. Never publish an empty directory —
  // a stale-but-valid directory that expires on its own is strictly better than
  // signing "there are no relays" (which fails every client closed immediately).
  if (relays.length === 0) {
    reconcileFailures += 1;
    console.error("no healthy relays this cycle — leaving the previous directory in place to expire on its own");
  } else {
    await publishDirectory(relays);
  }

  await emitMetrics(perRegionHealthy, reconcileFailures);

  return { published: relays.length > 0, healthy: perRegionHealthy, placement, reconcileFailures };
};

// --- Random region placement (see placement.mjs) -----------------------------

// Load the persisted draw and decide whether it still holds; re-draw + persist if
// not. Returns a region -> node-count map covering this rotation.
async function resolvePlacement() {
  const allowedRegions = Object.keys(REGIONS);
  const total = clamp(await readDesiredTotalFromSsm(), NODE_FLOOR, NODE_MAX);
  const now = Math.floor(Date.now() / 1000);

  let current = null;
  try {
    current = JSON.parse(await readParam(PLACEMENT_PARAM, false));
  } catch (err) {
    console.error("failed to read placement parameter, will re-draw:", err);
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

async function readDesiredTotalFromSsm() {
  try {
    return readDesiredTotal(await readParam(DESIRED_STATE_PARAM, false), NODE_FLOOR);
  } catch (err) {
    console.error("failed to read desired-state, falling back to the floor:", err);
    return NODE_FLOOR;
  }
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

async function setDesiredCapacity(region, asgName, target) {
  const asg = new AutoScalingClient({ region });
  await asg.send(new UpdateAutoScalingGroupCommand({
    AutoScalingGroupName: asgName,
    DesiredCapacity: target,
  }));
  console.log(`${region}: set ASG ${asgName} desired capacity -> ${target}`);
}

async function discoverInServiceNodes(region, asgName) {
  const asg = new AutoScalingClient({ region });
  const ec2 = new EC2Client({ region });

  const groups = await asg.send(new DescribeAutoScalingGroupsCommand({
    AutoScalingGroupNames: [asgName],
  }));
  const group = groups.AutoScalingGroups?.[0];
  if (!group) {
    return [];
  }

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
async function markUnhealthy(region, unhealthyNodes) {
  const asg = new AutoScalingClient({ region });
  for (const { instanceId } of unhealthyNodes) {
    try {
      await asg.send(new SetInstanceHealthCommand({
        InstanceId: instanceId,
        HealthStatus: "Unhealthy",
        ShouldRespectGracePeriod: true,
      }));
      console.log(`${region}: marked ${instanceId} Unhealthy (no /version) — ASG will replace it`);
    } catch (err) {
      console.error(`${region}: failed to mark ${instanceId} unhealthy:`, err);
    }
  }
}

// --- Health check ------------------------------------------------------------

async function healthCheck(nodes) {
  const results = await Promise.all(nodes.map(async (node) => {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), HEALTH_TIMEOUT_MS);
    try {
      // Treat any 200 as healthy; parse the JSON only if we want the SHA.
      const res = await fetch(`http://${node.ip}:${HEALTH_PORT}/version`, { signal: controller.signal });
      return { node, ok: res.ok };
    } catch {
      return { node, ok: false };
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

async function emitMetrics(perRegionHealthy, reconcileFailures) {
  const timestamp = new Date();
  const data = [
    { MetricName: "ReconcileFailures", Value: reconcileFailures, Unit: "Count", Timestamp: timestamp },
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
