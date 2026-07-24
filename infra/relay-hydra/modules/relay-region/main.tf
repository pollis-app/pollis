# A per-region pool shard: minimal public-subnet VPC + a locked security group +
# a mixed-instances (on-demand floor + Spot) ASG of stateless t4g.nano relay
# nodes. No LB, no NAT gateway, no private subnets — nodes have direct public
# egress and a public IP:udp-port the client dials. The relay binary's own
# allowlist is the egress control (SGs can't filter by hostname anyway).

terraform {
  required_providers {
    aws = {
      source                = "hashicorp/aws"
      configuration_aliases = []
    }
  }
}

data "aws_availability_zones" "available" {
  state = "available"
}

locals {
  azs         = slice(data.aws_availability_zones.available.names, 0, var.az_count)
  name        = "pollis-relay-${var.region}"
  vpc_cidr    = "10.20.0.0/16"
  subnet_bits = 8
}

# Latest Amazon Linux 2023 arm64 AMI (Graviton) — resolved at plan time.
data "aws_ssm_parameter" "al2023_arm64" {
  name = "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-arm64"
}

# ── Network ─────────────────────────────────────────────────────────────────

resource "aws_vpc" "this" {
  cidr_block           = local.vpc_cidr
  enable_dns_support   = true
  enable_dns_hostnames = true
  tags                 = { Name = local.name }
}

resource "aws_internet_gateway" "this" {
  vpc_id = aws_vpc.this.id
  tags   = { Name = local.name }
}

// Keyed by AZ INDEX, not AZ name: the name list comes from a data source and is
// unknown at plan time, and for_each keys must be static. The apply-time AZ name
// therefore goes in the value position (see the `availability_zone` attribute) —
// which is what Terraform's "unknown values in for_each" guidance prescribes.
resource "aws_subnet" "public" {
  for_each = toset([for idx in range(var.az_count) : tostring(idx)])

  vpc_id                  = aws_vpc.this.id
  availability_zone       = local.azs[tonumber(each.key)]
  cidr_block              = cidrsubnet(local.vpc_cidr, local.subnet_bits, tonumber(each.key))
  map_public_ip_on_launch = true
  tags                    = { Name = "${local.name}-${local.azs[tonumber(each.key)]}" }
}

resource "aws_route_table" "public" {
  vpc_id = aws_vpc.this.id
  route {
    cidr_block = "0.0.0.0/0"
    gateway_id = aws_internet_gateway.this.id
  }
  tags = { Name = local.name }
}

resource "aws_route_table_association" "public" {
  for_each       = aws_subnet.public
  subnet_id      = each.value.id
  route_table_id = aws_route_table.public.id
}

# ── Security group — opens ONLY the relay UDP port + the health TCP port ─────

resource "aws_security_group" "relay" {
  name        = local.name
  description = "Pollis relay node: QUIC UDP from anywhere, health TCP scoped, egress open (allowlist is enforced in the binary)."
  vpc_id      = aws_vpc.this.id

  ingress {
    description = "QUIC relay (clients dial this)"
    from_port   = var.relay_port
    to_port     = var.relay_port
    protocol    = "udp"
    cidr_blocks = ["0.0.0.0/0"]
  }

  ingress {
    description = "Health/version probe"
    from_port   = var.health_port
    to_port     = var.health_port
    protocol    = "tcp"
    cidr_blocks = [var.health_source_cidr]
  }

  # Egress open by design (§4): SGs filter by IP/CIDR, not hostname, and the
  # first-party hosts sit behind rotating anycast IPs. POLLIS_RELAY_ALLOWLIST in
  # the binary IS the egress boundary. No SSH — shell access is via SSM only.
  egress {
    description = "Open egress (allowlist enforced in the relay binary)"
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }

  tags = { Name = local.name }
}

# ── IAM instance profile — least privilege ──────────────────────────────────

data "aws_iam_policy_document" "assume_ec2" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["ec2.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "node" {
  name               = "${local.name}-node"
  assume_role_policy = data.aws_iam_policy_document.assume_ec2.json
  tags               = { app = "pollis-relay" }
}

# SSM Session Manager for shell access (replaces SSH — nothing open to the world).
resource "aws_iam_role_policy_attachment" "ssm_core" {
  role       = aws_iam_role.node.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

data "aws_caller_identity" "current" {}

# Read ONLY the two QUIC identity params, and decrypt them via the SSM-scoped KMS key.
data "aws_iam_policy_document" "node_identity" {
  statement {
    sid     = "ReadQuicIdentity"
    actions = ["ssm:GetParameter", "ssm:GetParameters"]
    resources = [
      "arn:aws:ssm:${var.region}:${data.aws_caller_identity.current.account_id}:parameter${var.identity_key_param}",
      "arn:aws:ssm:${var.region}:${data.aws_caller_identity.current.account_id}:parameter${var.identity_cert_param}",
    ]
  }
  statement {
    sid       = "DecryptViaSsm"
    actions   = ["kms:Decrypt"]
    resources = ["*"]
    condition {
      test     = "StringEquals"
      variable = "kms:ViaService"
      values   = ["ssm.${var.region}.amazonaws.com"]
    }
  }
}

resource "aws_iam_role_policy" "node_identity" {
  name   = "quic-identity"
  role   = aws_iam_role.node.id
  policy = data.aws_iam_policy_document.node_identity.json
}

resource "aws_iam_instance_profile" "node" {
  name = "${local.name}-node"
  role = aws_iam_role.node.name
}

# ── Launch template + mixed-instances ASG ───────────────────────────────────

resource "aws_launch_template" "relay" {
  name_prefix   = "${local.name}-"
  image_id      = data.aws_ssm_parameter.al2023_arm64.value
  instance_type = var.instance_type

  iam_instance_profile {
    arn = aws_iam_instance_profile.node.arn
  }

  network_interfaces {
    associate_public_ip_address = true
    security_groups             = [aws_security_group.relay.id]
    delete_on_termination       = true
  }

  # Smallest sensible root volume — EBS is ~$0.6/mo at 8 GiB gp3.
  block_device_mappings {
    device_name = "/dev/xvda"
    ebs {
      volume_size           = 8
      volume_type           = "gp3"
      delete_on_termination = true
      encrypted             = true
    }
  }

  metadata_options {
    http_tokens   = "required" # IMDSv2 only
    http_endpoint = "enabled"
  }

  user_data = base64encode(templatefile("${path.module}/user-data.sh.tftpl", {
    region              = var.region
    relay_image         = var.relay_image
    relay_port          = var.relay_port
    health_port         = var.health_port
    relay_allowlist     = var.relay_allowlist
    identity_key_param  = var.identity_key_param
    identity_cert_param = var.identity_cert_param
  }))

  tag_specifications {
    resource_type = "instance"
    tags = {
      Name = local.name
      app  = "pollis-relay"
    }
  }

  tags = { app = "pollis-relay" }
}

resource "aws_autoscaling_group" "relay" {
  name                = local.name
  min_size            = var.node_floor
  max_size            = var.node_max
  desired_capacity    = var.node_floor
  vpc_zone_identifier = [for s in aws_subnet.public : s.id]
  health_check_type   = "EC2"

  mixed_instances_policy {
    instances_distribution {
      # Guarantee `node_floor` on-demand nodes; everything above is Spot. Spot
      # reclamation can therefore never take the pool below the floor.
      on_demand_base_capacity                  = var.node_floor
      on_demand_percentage_above_base_capacity = 0
      spot_allocation_strategy                 = "price-capacity-optimized"
      spot_max_price                           = var.spot_max_price
    }

    launch_template {
      launch_template_specification {
        launch_template_id = aws_launch_template.relay.id
        version            = "$Latest"
      }
      override {
        instance_type = var.instance_type
      }
    }
  }

  tag {
    key                 = "app"
    value               = "pollis-relay"
    propagate_at_launch = true
  }

  # The reconciler owns desired_capacity at runtime — don't fight it on apply.
  lifecycle {
    ignore_changes = [desired_capacity]
  }
}
