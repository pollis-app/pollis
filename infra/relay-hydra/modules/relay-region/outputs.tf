output "asg_name" {
  description = "Auto Scaling Group name the reconciler drives."
  value       = aws_autoscaling_group.relay.name
}

output "security_group_id" {
  value = aws_security_group.relay.id
}

output "vpc_id" {
  value = aws_vpc.this.id
}
