# The reconciler: a Node 24 Lambda on an EventBridge schedule that converges the
# pool to desired-state, draws the random region placement, health-checks nodes,
# and re-signs + publishes the directory. Least-privilege IAM; a handful of
# CloudWatch alarms (no dashboards).

terraform {
  required_providers {
    aws = {
      source = "hashicorp/aws"
    }
  }
}

data "aws_caller_identity" "current" {}

locals {
  function_name = "${var.name_prefix}-reconciler"
  metric_ns     = var.metric_namespace
}

# ── Package (zero deps: SDK v3 + node:crypto are in the runtime) ─────────────
# Zips the whole reconciler/ directory, so index.mjs + placement.mjs both ship.

data "archive_file" "reconciler" {
  type        = "zip"
  source_dir  = "${path.module}/../../reconciler"
  output_path = "${path.module}/../../.build/reconciler.zip"
}

# ── IAM ─────────────────────────────────────────────────────────────────────

data "aws_iam_policy_document" "assume_lambda" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["lambda.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "reconciler" {
  name               = local.function_name
  assume_role_policy = data.aws_iam_policy_document.assume_lambda.json
  tags               = { app = "pollis-relay" }
}

data "aws_iam_policy_document" "reconciler" {
  # Logs.
  statement {
    sid       = "Logs"
    actions   = ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"]
    resources = ["arn:aws:logs:${var.primary_region}:${data.aws_caller_identity.current.account_id}:*"]
  }

  # Read ASG state anywhere (no resource-level support for Describe).
  statement {
    sid       = "DescribeAsg"
    actions   = ["autoscaling:DescribeAutoScalingGroups"]
    resources = ["*"]
  }

  # Scale only pollis-relay ASGs; SetInstanceHealth lets the reconciler flag a
  # dead-container node so the ASG replaces it (self-heal).
  statement {
    sid       = "ScaleAsg"
    actions   = ["autoscaling:UpdateAutoScalingGroup", "autoscaling:SetDesiredCapacity", "autoscaling:SetInstanceHealth"]
    resources = ["*"]
    condition {
      test     = "StringEquals"
      variable = "autoscaling:ResourceTag/app"
      values   = ["pollis-relay"]
    }
  }

  # Discover node public IPs (no resource-level support).
  statement {
    sid       = "DescribeInstances"
    actions   = ["ec2:DescribeInstances"]
    resources = ["*"]
  }

  # Read the desired-state, the current placement, and the signing/identity secrets.
  statement {
    sid       = "ReadParams"
    actions   = ["ssm:GetParameter", "ssm:GetParameters"]
    resources = concat(var.secret_param_arns, [var.desired_state_param_arn, var.placement_param_arn])
  }

  # Persist a fresh random draw. Scoped to the placement parameter ALONE — the
  # reconciler must never be able to rewrite desired-state or any secret.
  statement {
    sid       = "WritePlacement"
    actions   = ["ssm:PutParameter"]
    resources = [var.placement_param_arn]
  }

  statement {
    sid       = "DecryptViaSsm"
    actions   = ["kms:Decrypt"]
    resources = ["*"]
    condition {
      test     = "StringEquals"
      variable = "kms:ViaService"
      values   = ["ssm.${var.primary_region}.amazonaws.com"]
    }
  }

  # Publish the signed directory.
  statement {
    sid       = "PublishDirectory"
    actions   = ["s3:PutObject"]
    resources = ["${var.directory_bucket_arn}/${var.directory_object_key}"]
  }

  # Metrics (no resource-level support).
  statement {
    sid       = "Metrics"
    actions   = ["cloudwatch:PutMetricData"]
    resources = ["*"]
    condition {
      test     = "StringEquals"
      variable = "cloudwatch:namespace"
      values   = [local.metric_ns]
    }
  }
}

resource "aws_iam_role_policy" "reconciler" {
  name   = "reconciler"
  role   = aws_iam_role.reconciler.id
  policy = data.aws_iam_policy_document.reconciler.json
}

# ── Function ────────────────────────────────────────────────────────────────

resource "aws_cloudwatch_log_group" "reconciler" {
  name              = "/aws/lambda/${local.function_name}"
  retention_in_days = 14
  tags              = { app = "pollis-relay" }
}

resource "aws_lambda_function" "reconciler" {
  function_name = local.function_name
  role          = aws_iam_role.reconciler.arn
  # Node 20 reached EOL 2026-04-30 and Lambda stopped patching it the same day
  # (create blocked 2027-02-01, update blocked 2027-03-03). Node 24 is the current
  # active-LTS Lambda runtime, supported to ~April 2028 — chosen over 22 (EOL
  # 2027-04) so this doesn't come round again inside a year.
  runtime          = "nodejs24.x"
  handler          = "index.handler"
  filename         = data.archive_file.reconciler.output_path
  source_code_hash = data.archive_file.reconciler.output_base64sha256
  timeout          = 60
  memory_size      = 256
  architectures    = ["arm64"]

  environment {
    variables = {
      MANAGED_REGIONS         = jsonencode(var.managed_regions)
      DESIRED_STATE_PARAM     = var.desired_state_param
      PLACEMENT_PARAM         = var.placement_param
      SIGNING_KEY_PARAM       = var.signing_key_param
      IDENTITY_CERT_PARAM     = var.identity_cert_param
      DIRECTORY_BUCKET        = var.directory_bucket
      DIRECTORY_KEY           = var.directory_object_key
      RELAY_PORT              = tostring(var.relay_port)
      HEALTH_PORT             = tostring(var.health_port)
      NODE_FLOOR              = tostring(var.node_floor)
      NODE_MAX                = tostring(var.node_max)
      ROTATION_INTERVAL_HOURS = tostring(var.rotation_interval_hours)
      DIRECTORY_TTL_SECONDS   = tostring(var.directory_ttl_seconds)
      METRIC_NAMESPACE        = local.metric_ns
    }
  }

  depends_on = [aws_cloudwatch_log_group.reconciler]
  tags       = { app = "pollis-relay" }
}

# ── Schedule ────────────────────────────────────────────────────────────────

resource "aws_cloudwatch_event_rule" "schedule" {
  name                = "${local.function_name}-schedule"
  schedule_expression = var.reconcile_schedule
  tags                = { app = "pollis-relay" }
}

resource "aws_cloudwatch_event_target" "schedule" {
  rule      = aws_cloudwatch_event_rule.schedule.name
  target_id = "reconciler"
  arn       = aws_lambda_function.reconciler.arn
}

resource "aws_lambda_permission" "events" {
  statement_id  = "AllowEventBridge"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.reconciler.function_name
  principal     = "events.amazonaws.com"
  source_arn    = aws_cloudwatch_event_rule.schedule.arn
}

# ── Alarm notifications: SNS topic + email subscriptions ────────────────────
# The alarms below all route to this topic. Email subscriptions require a one-time
# confirmation click in the mail AWS sends (Terraform can't auto-confirm — the
# subscription sits "pending confirmation" until then).

resource "aws_sns_topic" "alarms" {
  name = "${local.function_name}-alarms"
  tags = { app = "pollis-relay" }
}

resource "aws_sns_topic_subscription" "alarm_emails" {
  for_each  = toset(var.alarm_email_addresses)
  topic_arn = aws_sns_topic.alarms.arn
  protocol  = "email"
  endpoint  = each.value
}

# Let CloudWatch alarms publish to the topic.
data "aws_iam_policy_document" "alarms_topic" {
  statement {
    sid       = "AllowCloudWatchAlarmsPublish"
    actions   = ["sns:Publish"]
    resources = [aws_sns_topic.alarms.arn]
    principals {
      type        = "Service"
      identifiers = ["cloudwatch.amazonaws.com"]
    }
    condition {
      test     = "StringEquals"
      variable = "AWS:SourceAccount"
      values   = [data.aws_caller_identity.current.account_id]
    }
  }
}

resource "aws_sns_topic_policy" "alarms" {
  arn    = aws_sns_topic.alarms.arn
  policy = data.aws_iam_policy_document.alarms_topic.json
}

# ── Alarms (a handful; $0.10 each) ──────────────────────────────────────────
# Each fires AND recovers to the SNS topic (alarm_actions + ok_actions) so a
# resolved issue sends an all-clear, not silence.

resource "aws_cloudwatch_metric_alarm" "reconcile_failures" {
  alarm_name          = "${local.function_name}-reconcile-failures"
  comparison_operator = "GreaterThanThreshold"
  evaluation_periods  = 2
  metric_name         = "ReconcileFailures"
  namespace           = local.metric_ns
  period              = 300
  statistic           = "Maximum"
  threshold           = 0
  treat_missing_data  = "notBreaching"
  alarm_actions       = [aws_sns_topic.alarms.arn]
  ok_actions          = [aws_sns_topic.alarms.arn]
  tags                = { app = "pollis-relay" }
}

resource "aws_cloudwatch_metric_alarm" "lambda_errors" {
  alarm_name          = "${local.function_name}-lambda-errors"
  comparison_operator = "GreaterThanThreshold"
  evaluation_periods  = 2
  metric_name         = "Errors"
  namespace           = "AWS/Lambda"
  period              = 300
  statistic           = "Sum"
  threshold           = 0
  treat_missing_data  = "notBreaching"
  dimensions          = { FunctionName = aws_lambda_function.reconciler.function_name }
  alarm_actions       = [aws_sns_topic.alarms.arn]
  ok_actions          = [aws_sns_topic.alarms.arn]
  tags                = { app = "pollis-relay" }
}

# POOL-WIDE healthy node count fell below the floor (missing data also breaches —
# no metric emitted means the reconciler isn't running).
#
# Deliberately pool-wide, not per-region. Random placement means an individual
# region legitimately holds zero nodes for a whole rotation, so the old
# per-region alarm would page on every draw that skipped a region — and with a
# shard now standing by in all four regions, most draws skip at least one.
resource "aws_cloudwatch_metric_alarm" "healthy_nodes_total" {
  alarm_name          = "${local.function_name}-healthy-nodes-total"
  comparison_operator = "LessThanThreshold"
  evaluation_periods  = 3
  metric_name         = "HealthyNodesTotal"
  namespace           = local.metric_ns
  period              = 300
  statistic           = "Minimum"
  threshold           = var.node_floor
  treat_missing_data  = "breaching"
  alarm_actions       = [aws_sns_topic.alarms.arn]
  ok_actions          = [aws_sns_topic.alarms.arn]
  tags                = { app = "pollis-relay" }
}

# This replaces the previous per-region `aws_cloudwatch_metric_alarm.healthy_nodes`
# (for_each over managed_regions). Dropping that block from the config is enough —
# Terraform destroys it on the next apply. Leaving it in place would have meant an
# alarm stuck in ALARM for every region the draw happened to skip.
