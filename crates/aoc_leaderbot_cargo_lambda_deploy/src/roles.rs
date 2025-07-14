use aoc_leaderbot_cargo_lambda_interactive::progress::Progress;
use aoc_leaderbot_cargo_lambda_remote::aws_sdk_config::SdkConfig;
use aws_sdk_iam::Client as IamClient;
use aws_sdk_sts::{Client as StsClient, Error};
use aws_smithy_types::error::metadata::ProvideErrorMetadata;
use miette::{IntoDiagnostic, Result, WrapErr};
use tokio::time::{Duration, sleep};

const BASIC_LAMBDA_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

#[derive(Debug)]
pub(crate) struct FunctionRole(String, bool);

impl FunctionRole {
    /// Create a new function role.
    pub(crate) fn new(arn: String) -> FunctionRole {
        FunctionRole(arn, true)
    }

    /// Create a function role from an existing role.
    pub(crate) fn from_existing(arn: String) -> FunctionRole {
        FunctionRole(arn, false)
    }

    pub(crate) fn arn(&self) -> &str {
        &self.0
    }

    pub(crate) fn is_new(&self) -> bool {
        self.1
    }
}

pub(crate) async fn create(config: &SdkConfig, progress: &Progress) -> Result<FunctionRole> {
    progress.set_message("creating execution role");

    let role_name = format!("cargo-lambda-role-{}", uuid::Uuid::new_v4());
    let client = IamClient::new(config);
    let sts_client = StsClient::new(config);
    let identity = sts_client
        .get_caller_identity()
        .send()
        .await
        .into_diagnostic()
        .wrap_err("failed to get caller identity")?;

    let mut policy = serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["sts:AssumeRole"],
                "Principal": {
                    "Service": "lambda.amazonaws.com"
                }
            },
            {
                "Effect": "Allow",
                "Action": ["sts:AssumeRole", "sts:SetSourceIdentity", "sts:TagSession"],
                "Principal": {
                    "AWS": identity.arn().expect("missing account arn"),
                }
            }
        ]
    });

    tracing::trace!(policy = ?policy, "creating role with assume policy");

    let role = client
        .create_role()
        .role_name(&role_name)
        .assume_role_policy_document(policy.to_string())
        .send()
        .await
        .into_diagnostic()
        .wrap_err("failed to create function role")?
        .role
        .expect("missing role information");

    client
        .attach_role_policy()
        .role_name(&role_name)
        .policy_arn(BASIC_LAMBDA_EXECUTION_POLICY)
        .send()
        .await
        .into_diagnostic()
        .wrap_err("failed to attach policy AWSLambdaBasicExecutionRole to function role")?;

    let role_arn = role.arn();

    progress.set_message("verifying role access, this can take up to 20 seconds");

    try_assume_role(&sts_client, role_arn).await?;

    // remove the current identity from the trust policy
    policy["Statement"]
        .as_array_mut()
        .expect("missing statement array")
        .pop();

    tracing::trace!(policy = ?policy, "updating assume policy");

    client
        .update_assume_role_policy()
        .role_name(&role_name)
        .policy_document(policy.to_string())
        .send()
        .await
        .into_diagnostic()
        .wrap_err("failed to restrict service policy")?;

    tracing::debug!(role = ?role, "function role created");

    Ok(FunctionRole::new(role_arn.to_string()))
}

async fn try_assume_role(client: &StsClient, role_arn: &str) -> Result<()> {
    sleep(Duration::from_secs(5)).await;

    for attempt in 1..3 {
        let session_id = format!(
            "aoc_leaderbot_cargo_lambda_session_{}",
            uuid::Uuid::new_v4()
        );

        let result = client
            .assume_role()
            .role_arn(role_arn)
            .role_session_name(session_id)
            .send()
            .await
            .map_err(Error::from);

        tracing::trace!(attempt = attempt, result = ?result, "attempted to assume new role");

        match result {
            Ok(_) => return Ok(()),
            Err(err) if attempt < 3 => match err.code() {
                Some("AccessDenied") => {
                    tracing::trace!(
                        ?err,
                        "role might not be fully propagated yet, waiting before retrying"
                    );
                    sleep(Duration::from_secs(attempt * 5)).await
                }
                _ => {
                    return Err(err)
                        .into_diagnostic()
                        .wrap_err("failed to assume new lambda role");
                }
            },
            Err(err) => {
                return Err(err)
                    .into_diagnostic()
                    .wrap_err("failed to assume new lambda role");
            }
        }
    }

    Err(miette::miette!(
        "failed to assume new lambda role.\nTry deploying using the flag `--iam-role {}`",
        role_arn
    ))
}
