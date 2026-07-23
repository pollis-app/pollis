output "function_name" {
  value = aws_lambda_function.reconciler.function_name
}

output "function_arn" {
  value = aws_lambda_function.reconciler.arn
}

output "role_arn" {
  value = aws_iam_role.reconciler.arn
}
