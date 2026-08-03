terraform {
  required_version = ">= 1.6.0"

  required_providers {
    # 6.x, not 5.x: the 5.x line stopped at 5.100 and its runtime validator
    # predates nodejs24.x, so it rejects the runtime this Lambda now needs.
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"
    }
    archive = {
      source  = "hashicorp/archive"
      version = "~> 2.4"
    }
  }

  # Remote state is optional for a stack this small. To share state across
  # operators, uncomment and point at an S3 bucket + DynamoDB lock table you own:
  #
  # backend "s3" {
  #   bucket         = "pollis-tfstate"
  #   key            = "relay-hydra/terraform.tfstate"
  #   region         = "us-west-2"
  #   dynamodb_table = "pollis-tfstate-lock"
  #   encrypt        = true
  # }
}
