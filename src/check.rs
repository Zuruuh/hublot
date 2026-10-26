use std::{collections::HashMap, fs, sync::Arc};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use tokio::{sync::Semaphore, task::JoinSet};
use tracing::{error, info, warn};

use crate::{
    config::Config,
    db::{self, MODE_DIGEST, MODE_SEMVER},
    reference::{parse_image_file, ImageRef},
    registry::RegistryClient,
    site,
    version::{is_semver_like, select_candidates, Candidates},
};

#[derive(Debug, Clone, Copy)]
pub struct CheckReport {
    pub image_count: usize,
    pub error_count: usize,
}

enum CheckOutcome {
    Semver(Candidates),
    Digest(String),
}

pub async fn run(config: &Config) -> Result<CheckReport> {
    let contents = fs::read_to_string(&config.images_file)
        .with_context(|| format!("failed to read image file {}", config.images_file.display()))?;
    let images = parse_image_file(&contents)?;

    if images.is_empty() {
        warn!(
            path = %config.images_file.display(),
            "image file contains no image references"
        );
    }

    let connection = db::open(&config.database)?;
    db::init(&connection)?;
    db::deactivate_all(&connection)?;

    let now = Utc::now().timestamp();
    let run_id = db::start_run(&connection, now)?;
    let mut records = HashMap::new();
    for image in &images {
        let mode = if is_semver_like(&image.tag) {
            MODE_SEMVER
        } else {
            MODE_DIGEST
        };
        let record = db::upsert_image(&connection, image, mode, now)?;
        records.insert(image.canonical(), record);
    }

    let image_count = images.len();
    let client = Arc::new(RegistryClient::new(config)?);
    let semaphore = Arc::new(Semaphore::new(config.check.concurrency));
    let mut tasks = JoinSet::new();

    for image in images {
        let client = Arc::clone(&client);
        let semaphore = Arc::clone(&semaphore);
        tasks.spawn(async move {
            let permit = match semaphore.acquire_owned().await {
                Ok(permit) => permit,
                Err(error) => {
                    return (image, Err(anyhow::Error::new(error)));
                }
            };
            let result = check_image(&client, &image).await;
            drop(permit);
            (image, result)
        });
    }

    let mut error_count = 0usize;
    while let Some(joined) = tasks.join_next().await {
        let (image, result) = joined.context("image check task failed")?;
        let record = records
            .get(&image.canonical())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("image record disappeared during check"))?;

        match result {
            Ok(CheckOutcome::Semver(candidates)) => {
                info!(
                    image = %image.canonical(),
                    compatible = candidates.compatible.as_deref().unwrap_or("-"),
                    latest = candidates.latest.as_deref().unwrap_or("-"),
                    "checked semver image"
                );
                db::record_semver(
                    &connection,
                    &record,
                    candidates.compatible.as_deref(),
                    candidates.latest.as_deref(),
                    Utc::now().timestamp(),
                )?;
            }
            Ok(CheckOutcome::Digest(digest)) => {
                info!(
                    image = %image.canonical(),
                    digest = %digest,
                    "checked digest image"
                );
                db::record_digest(&connection, &record, &digest, Utc::now().timestamp())?;
            }
            Err(check_error) => {
                error_count += 1;
                error!(
                    image = %image.canonical(),
                    error = %format!("{check_error:#}"),
                    "image check failed"
                );
                db::record_error(
                    &connection,
                    record.id,
                    &format!("{check_error:#}"),
                    Utc::now().timestamp(),
                )?;
            }
        }
    }

    let status = if error_count == 0 {
        "success"
    } else {
        "partial"
    };
    db::finish_run(
        &connection,
        run_id,
        status,
        image_count as i64,
        error_count as i64,
        Utc::now().timestamp(),
    )?;
    drop(connection);

    site::write_site(config)?;

    Ok(CheckReport {
        image_count,
        error_count,
    })
}

async fn check_image(client: &RegistryClient, image: &ImageRef) -> Result<CheckOutcome> {
    if is_semver_like(&image.tag) {
        let tags = client.list_tags(image).await?;
        if !tags.iter().any(|tag| tag == &image.tag) {
            bail!(
                "current tag {} was not found in the registry tag list",
                image.tag
            );
        }
        let candidates = select_candidates(&image.tag, &tags)
            .ok_or_else(|| anyhow::anyhow!("failed to parse semver tag {}", image.tag))?;
        Ok(CheckOutcome::Semver(candidates))
    } else {
        let digest = client.manifest_digest(image).await?;
        Ok(CheckOutcome::Digest(digest))
    }
}
