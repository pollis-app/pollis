// Pollis relay-pool reconciler ("the hydra") — issue #616.
//
// Runs as a Node.js 20 Lambda on an EventBridge schedule (and on-demand).
// Each run is idempotent and converges reality to the desired-state config:
//   1. Read desired-state (region -> node count) from SSM Parameter Store.
//   2. For each managed region, set the ASG desired capacity (clamped to
//      [floor, max]) so Spot reclamation can never take the pool to zero.
//   3. Discover the ASG's InService instances and their public IPs.
//   4. Health-check each node at GET http://<ip>:<health-port>/version and
//      keep only the ones that answer 200.
//   5. Assemble the healthy set into the signed Directory (§3 of the ticket),
//      sign the exact payload bytes with the Ed25519 private key from SSM, and
//      publish the envelope to S3 (CloudFront serves it at the stable URL).
//   6. Emit CloudWatch metrics (healthy nodes per region, reconcile failures).
//
// Zero third-party deps: the AWS SDK v3 clients and node:crypto are provided by
// the Lambda Node 20 runtime. The Ed25519 signature is produced with
// crypto.sign(null, ...), i.e. pure EdDSA over the raw payload bytes — the exact
// bytes the client base64-decodes and verifies (see scripts/verify-directory.mjs
// and test/directory-contract.test.mjs, which run the client's verification path).

import { AutoScalingClient, DescribeAutoScalingGroupsCommand, UpdateAutoScalingGroupCommand } from "@aws-sdk/client-auto-scaling";
import { EC2Client, DescribeInstancesCommand } from "@aws-sdk/client-ec2";
import { SSMClient, GetParameterCommand } from "@aws-sdk/client-ssm";
import { S3Client, PutObjectCommand } from "@aws-sdk/client-s3";
import { CloudWatchClient, PutMetricDataCommand } from "@aws-sdk/client-cloudwatch";
import { createPrivateKey, sign } from "node:crypto";

// --- Config from the Lambda environment (set by Terraform) -------------------

const REGIONS = JSON.parse(env("MANAGED_REGIONS")); // { "us-west-2": "<asg-name>" }
const DESIRED_STATE_PARAM = env("DESIRED_STATE_PARAM"); // SSM param holding {region:count}
const SIGNING_KEY_PARAM = env("SIGNING_KEY_PARAM"); // SSM SecureString: Ed25519 private PKCS8 PEM
const IDENTITY_CERT_PARAM = env("IDENTITY_CERT_PARAM"); // SSM SecureString: base64(DER) of the pool QUIC cert
const DIRECTORY_BUCKET = env("DIRECTORY_BUCKET");
const DIRECTORY_KEY = env("DIRECTORY_KEY"); // S3 object key, e.g. "directory.json"
const RELAY_PORT = Number(env("RELAY_PORT", "9444"));
const HEALTH_PORT = Number(env("HEALTH_PORT", "9445"));
const NODE_FLOOR = Number(env("NODE_FLOOR", "2"));
const NODE_MAX = Number(env("NODE_MAX", "3"));
const DIRECTORY_TTL_SECONDS = Number(env("DIRECTORY_TTL_SECONDS", "3600"));
const HEALTH_TIMEOUT_MS = Number(env("HEALTH_TIMEOUT_MS", "2500"));
const METRIC_NAMESPACE = env("METRIC_NAMESPACE", "PollisRelayHydra");

const ssm = new SSMClient({});
const cw = new CloudWatchClient({});

export const handler = async () => {
  let reconcileFailures = 0;
  const perRegionHealthy = {};
  const relays = [];

  const desired = await readDesiredState();
  const certB64 = await readParam(IDENTITY_CERT_PARAM, true);

  for (const [region, asgName] of Object.entries(REGIONS)) {
    try {
      const target = clamp(desired[region] ?? NODE_FLOOR, NODE_FLOOR, NODE_MAX);
      await setDesiredCapacity(region, asgName, target);

      const nodes = await discoverInServiceNodes(region, asgName);
      const healthy = await healthCheck(nodes);
      perRegionHealthy[region] = healthy.length;

      for (const ip of healthy) {
        relays.push({ addr: `${ip}:${RELAY_PORT}`, region, cert_b64: certB64 });
      }
    } catch (err) {
      reconcileFailures += 1;
      console.error(`reconcile failed for ${region}:`, err);
    }
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

  return { published: relays.length > 0, healthy: perRegionHealthy, reconcileFailures };
};

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
  const ips = [];
  for (const reservation of desc.Reservations ?? []) {
    for (const inst of reservation.Instances ?? []) {
      if (inst.PublicIpAddress) {
        ips.push(inst.PublicIpAddress);
      }
    }
  }
  return ips;
}

// --- Health check ------------------------------------------------------------

async function healthCheck(ips) {
  const results = await Promise.all(ips.map(async (ip) => {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), HEALTH_TIMEOUT_MS);
    try {
      // Treat any 200 as healthy; parse the JSON only if we want the SHA.
      const res = await fetch(`http://${ip}:${HEALTH_PORT}/version`, { signal: controller.signal });
      return res.ok ? ip : null;
    } catch {
      return null;
    } finally {
      clearTimeout(timer);
    }
  }));
  return results.filter(Boolean);
}

// --- Metrics -----------------------------------------------------------------

async function emitMetrics(perRegionHealthy, reconcileFailures) {
  const timestamp = new Date();
  const data = [
    { MetricName: "ReconcileFailures", Value: reconcileFailures, Unit: "Count", Timestamp: timestamp },
  ];
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

async function readDesiredState() {
  try {
    const raw = await readParam(DESIRED_STATE_PARAM, false);
    return JSON.parse(raw);
  } catch (err) {
    console.error("failed to read desired-state, falling back to floor everywhere:", err);
    return {};
  }
}

async function readParam(name, decrypt) {
  const out = await ssm.send(new GetParameterCommand({ Name: name, WithDecryption: decrypt }));
  return out.Parameter.Value;
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

function clamp(n, lo, hi) {
  return Math.max(lo, Math.min(hi, n));
}
