# The reconciler: a Node 20 Lambda on an EventBridge schedule that converges the
# pool to desired-state, health-checks nodes, and re-signs + publishes the
# directory. Least-privilege IAM; a handful of CloudWatch alarms (no dashboards).

terraform {
  required_providers {
    aws = {
      source = "hashicorp/aws"
    }
  }
}

data "aws_caller_identity" "current" {}

locals {
  function_name = "pollis-relay-hydra-reconciler"
  metric_ns     = "PollisRelayHydra"
}

# ── Package (zero deps: SDK v3 + node:crypto are in the runtime) ─────────────

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

  # Scale only pollis-relay ASGs.
  statement {
    sid       = "ScaleAsg"
    actions   = ["autoscaling:UpdateAutoScalingGroup", "autoscaling:SetDesiredCapacity"]
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

  # Read the desired-state + the signing/identity secrets.
  statement {
    sid       = "ReadParams"
    actions   = ["ssm:GetParameter", "ssm:GetParameters"]
    resources = concat(var.secret_param_arns, [var.desired_state_param_arn])
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
  function_name    = local.function_name
  role             = aws_iam_role.reconciler.arn
  runtime          = "nodejs20.x"
  handler          = "index.handler"
  filename         = data.archive_file.reconciler.output_path
  source_code_hash = data.archive_file.reconciler.output_base64sha256
  timeout          = 60
  memory_size      = 256
  architectures    = ["arm64"]

  environment {
    variables = {
      MANAGED_REGIONS       = jsonencode(var.managed_regions)
      DESIRED_STATE_PARAM   = var.desired_state_param
      SIGNING_KEY_PARAM     = var.signing_key_param
      IDENTITY_CERT_PARAM   = var.identity_cert_param
      DIRECTORY_BUCKET      = var.directory_bucket
      DIRECTORY_KEY         = var.directory_object_key
      RELAY_PORT            = tostring(var.relay_port)
      HEALTH_PORT           = tostring(var.health_port)
      NODE_FLOOR            = tostring(var.node_floor)
      NODE_MAX              = tostring(var.node_max)
      DIRECTORY_TTL_SECONDS = tostring(var.directory_ttl_seconds)
      METRIC_NAMESPACE      = local.metric_ns
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

# ── Alarms (a handful; $0.10 each) ──────────────────────────────────────────

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
  tags                = { app = "pollis-relay" }
}

# Per-region: healthy node count fell to zero (missing data also breaches — no
# metric emitted means the reconciler isn't running).
resource "aws_cloudwatch_metric_alarm" "healthy_nodes" {
  for_each = var.managed_regions

  alarm_name          = "${local.function_name}-healthy-nodes-${each.key}"
  comparison_operator = "LessThanThreshold"
  evaluation_periods  = 3
  metric_name         = "HealthyNodes"
  namespace           = local.metric_ns
  period              = 300
  statistic           = "Minimum"
  threshold           = 1
  treat_missing_data  = "breaching"
  dimensions          = { Region = each.key }
  tags                = { app = "pollis-relay" }
}
