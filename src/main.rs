use lambda_runtime::{run, service_fn, Error, LambdaEvent};
use serde_json::{json, Value};
use aws_config::meta::region::RegionProviderChain;
use aws_sdk_ec2::{Client as Ec2Client, types::Filter};
use std::env;
use tracing::{info, error};
use tracing_subscriber;

async fn handler(event: LambdaEvent<Value>) -> Result<Value, Error> {
    let (payload, _context) = event.into_parts();
    info!("Received event: {}", payload);

    let region_provider = RegionProviderChain::default_provider().or_else("us-east-1");
    let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(region_provider)
        .load()
        .await;
    let ec2_client = Ec2Client::new(&config);

    let instance_tag_name = env::var("INSTANCE_TAG_NAME")
        .unwrap_or_else(|_| "free-tier-al2023-k3s".to_string());
    let public_ip = env::var("EIP_PUBLIC_IP")
        .unwrap_or_else(|_| "98.91.128.103".to_string());

    info!("Instance tag name: {}, EIP: {}", instance_tag_name, public_ip);

    let desc = ec2_client
        .describe_instances()
        .filters(
            Filter::builder()
                .name("tag:Name")
                .values(instance_tag_name.clone())
                .build(),
        )
        .send()
        .await
        .map_err(|e| {
            error!("Failed to describe instances: {}", e);
            Error::from(e)
        })?;

    let instance_id = desc
        .reservations()
        .first()
        .and_then(|res| res.instances().first())
        .and_then(|i| i.instance_id())
        .ok_or_else(|| {
            let msg = format!("No instance found with tag Name={}", instance_tag_name);
            error!("{}", msg);
            msg
        })?
        .to_string();

    info!("Found Instance ID: {}", instance_id);

    let addr = ec2_client
        .describe_addresses()
        .public_ips(public_ip.clone())
        .send()
        .await
        .map_err(|e| {
            error!("Failed to describe addresses: {}", e);
            Error::from(e)
        })?;

    let allocation_id = addr
        .addresses()
        .first()
        .and_then(|a| a.allocation_id())
        .ok_or_else(|| {
            let msg = format!("No allocation found for EIP {}", public_ip);
            error!("{}", msg);
            msg
        })?
        .to_string();

    info!("Found Allocation ID: {}", allocation_id);

    let inst_desc = ec2_client
        .describe_instances()
        .instance_ids(instance_id.clone())
        .send()
        .await
        .map_err(|e| {
            error!("Failed to describe instance {}: {}", instance_id, e);
            Error::from(e)
        })?;

    let state: &str = inst_desc
        .reservations()
        .first()
        .and_then(|r| r.instances().first())
        .and_then(|i| i.state())
        .and_then(|s| s.name())
        .map(|name| name.as_str())
        .unwrap_or("unknown");

    info!("Instance {} state: {}", instance_id, state);

    if state == "running" {
        info!("Ensuring EIP attached to instance");
        ec2_client
            .associate_address()
            .instance_id(instance_id.clone())
            .allocation_id(allocation_id.clone())
            .allow_reassociation(true)
            .send()
            .await
            .map_err(|e| {
                error!("Failed to associate address: {}", e);
                Error::from(e)
            })?;
        info!("EIP associated successfully");
    } else {
        info!("Ensuring EIP detached from instance (by public IP)");
        ec2_client
            .disassociate_address()
            .public_ip(public_ip.clone())
            .send()
            .await
            .map_err(|e| {
                error!("Failed to disassociate address: {}", e);
                Error::from(e)
            })?;
        info!("EIP disassociated successfully");
    }

    Ok(json!({
        "instance_id": instance_id,
        "eip": public_ip,
        "state": state,
        "action": if state == "running" { "attached" } else { "detached" }
    }))
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_target(false)
        .without_time()
        .init();

    run(service_fn(handler)).await
}
