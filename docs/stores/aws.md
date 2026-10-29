### AWS Secrets Manager and Parameter Store

Saves each field to AWS Secrets Manager or to Systems Manager Parameter Store (as a `SecureString`)
through the `aws` CLI (v2), using the identity you're already signed in with (`aws configure` or
`aws sso login`). Each request is sent to `aws` as JSON on stdin (`--cli-input-json file:///dev/stdin`),
never on the command line. That uses `/dev/stdin`, so **macOS and Linux only for now**; on Windows the
check before the page opens fails with a message saying so.

Secrets Manager:

```json
{
  "title": "Stripe test keys",
  "reason": "The checkout service reads these from Secrets Manager.",
  "fields": [
    {"name": "STRIPE_SECRET_KEY", "label": "Secret key"},
    {"name": "STRIPE_WEBHOOK_SECRET", "label": "Webhook signing secret", "secret": "stripe/webhook"}
  ],
  "destination": {
    "store": {"type": "aws_secrets_manager", "region": "us-east-1"},
    "path": ".env.local"
  }
}
```

Parameter Store:

```json
{
  "title": "Stripe test keys",
  "reason": "The checkout service reads these from Parameter Store.",
  "fields": [
    {"name": "STRIPE_SECRET_KEY", "label": "Secret key", "secret": "/acme/prod/stripe-key"}
  ],
  "destination": {
    "store": {"type": "aws_parameter_store", "region": "us-east-1", "profile": "acme"},
    "path": ".env.local"
  }
}
```

With `path` set, the file receives the references below instead of the values, so it's safe to commit
or hand to an agent. Omit `path` to store only.

| option | default | meaning |
|---|---|---|
| `region` | the CLI's own (`AWS_REGION`, `AWS_DEFAULT_REGION`, or the profile's `region`) | AWS Region to store in (`--region`). |
| `profile` | the CLI's own (`AWS_PROFILE`, else `default`) | Named profile to use (`--profile`). |
| `kms_key_id` | the AWS managed key (`aws/secretsmanager`; for Parameter Store, the account's default SSM key) | Customer managed KMS key (ID, alias or ARN) to encrypt with. Secrets Manager applies it only when it creates a secret, not when it adds a version to an existing one. |

- Each field's name is its `secret` (or `name`). Secrets Manager names are 1-512 ASCII letters,
  digits or `/_+=.@-`. Parameter Store names are letters, digits and `_.-`, optionally as a path
  (`/app/prod/key`: leading `/`, at most 15 levels), and can't start with `aws` or `ssm`. An invalid
  name is rejected before the page opens.
- Secrets Manager: a missing secret is created, an existing one gets a new version (older versions
  are kept). Any other error (for example access denied) stops the save instead of being mistaken
  for "missing".
- Parameter Store: the parameter is created as a `SecureString` or overwritten (a new parameter
  version). Standard tier values are limited to 4 KB, Secrets Manager values to 64 KB.
- Before the page opens, agentikeys checks that `aws` is installed and signed in
  (`aws sts get-caller-identity`), that a region is set (spec, `AWS_REGION`/`AWS_DEFAULT_REGION`, or
  `aws configure get region`) and that every name is valid. It creates nothing at that point.

**IAM.** The signed-in identity needs:

- Secrets Manager: `secretsmanager:DescribeSecret`, `secretsmanager:CreateSecret` and
  `secretsmanager:PutSecretValue`. If every secret already exists, `DescribeSecret` +
  `PutSecretValue` is enough.
- Parameter Store: `ssm:PutParameter`.
- With a customer managed key (`kms_key_id`): `kms:GenerateDataKey` and `kms:Decrypt` on the key for
  Secrets Manager, `kms:Encrypt` on the key for Parameter Store. The default AWS managed keys need
  nothing extra.

**Reference** returned for each field:

- Secrets Manager: the secret's ARN, `arn:aws:secretsmanager:<region>:<account>:secret:<NAME>-<6 chars>`.
- Parameter Store: the parameter name, e.g. `/acme/prod/stripe-key` (PutParameter doesn't return an
  ARN; the region and account come from the same `region`/`profile`).

**Reading it back** (needs `secretsmanager:GetSecretValue` or `ssm:GetParameter`, plus `kms:Decrypt`
for a customer managed key):

```sh
aws secretsmanager get-secret-value --secret-id NAME --query SecretString --output text
aws ssm get-parameter --with-decryption --name NAME --query Parameter.Value --output text
```

Apps can resolve the same names with an AWS SDK, or reference them from ECS task definitions,
Lambda environment settings and similar.
