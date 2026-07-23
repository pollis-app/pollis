terraform {
  required_version = ">= 1.6.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.60"
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
