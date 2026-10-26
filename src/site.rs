use std::fs;

use anyhow::{Context, Result};
use askama::Template;
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::{
    config::Config,
    db::{
        self, EventRecord, ImageRecord, RecentEvent, RunRecord, STATUS_BASELINE, STATUS_ERROR,
        STATUS_UPDATE,
    },
};

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    css_path: &'static str,
    home_path: &'static str,
    generated_at: String,
    total_count: usize,
    update_count: usize,
    error_count: usize,
    baseline_count: usize,
    has_last_run: bool,
    last_run_time: String,
    last_run_status: String,
    last_run_images: i64,
    last_run_errors: i64,
    has_images: bool,
    images: Vec<ImageView>,
    has_events: bool,
    recent_events: Vec<RecentEventView>,
}

#[derive(Template)]
#[template(path = "detail.html")]
struct DetailTemplate {
    css_path: &'static str,
    home_path: &'static str,
    generated_at: String,
    image: ImageView,
    has_events: bool,
    events: Vec<EventView>,
}

struct ImageView {
    reference: String,
    tag: String,
    parsed_version: String,
    has_parsed_version: bool,
    mode_label: String,
    status_class: String,
    status_label: String,
    has_compatible: bool,
    compatible_tag: String,
    has_latest: bool,
    latest_tag: String,
    has_digest: bool,
    digest_short: String,
    has_error: bool,
    error: String,
    first_seen: String,
    last_checked: String,
    last_changed: String,
    detail_url: String,
}

struct EventView {
    kind_label: String,
    old_value: String,
    new_value: String,
    has_old: bool,
    has_new: bool,
    detected_at: String,
}

struct RecentEventView {
    reference: String,
    kind_label: String,
    old_value: String,
    new_value: String,
    has_old: bool,
    has_new: bool,
    detected_at: String,
}

#[derive(Serialize)]
struct StatusJson {
    generated_at: i64,
    last_run: Option<RunJson>,
    images: Vec<JsonImage>,
}

#[derive(Serialize)]
struct RunJson {
    started_at: i64,
    finished_at: Option<i64>,
    status: String,
    image_count: i64,
    error_count: i64,
}

#[derive(Serialize)]
struct JsonImage {
    id: i64,
    reference: String,
    registry: String,
    repository: String,
    tag: String,
    mode: String,
    parsed_version: Option<String>,
    status: String,
    current_digest: Option<String>,
    baseline_digest: Option<String>,
    compatible_tag: Option<String>,
    latest_tag: Option<String>,
    last_checked: Option<i64>,
    last_changed: Option<i64>,
    last_error: Option<String>,
}

pub fn write_site(config: &Config) -> Result<()> {
    let connection = db::open(&config.database)?;
    db::init(&connection)?;

    let images = db::list_active_images(&connection)?;
    let last_run = db::last_run(&connection)?;
    let recent_events = db::recent_events(&connection, 20)?;
    let generated_at = Utc::now();
    let generated_at_display = format_timestamp(Some(generated_at.timestamp()));

    let image_views: Vec<ImageView> = images.iter().map(ImageView::from_record).collect();

    let index = IndexTemplate {
        css_path: "style.css",
        home_path: "index.html",
        generated_at: generated_at_display.clone(),
        total_count: image_views.len(),
        update_count: images
            .iter()
            .filter(|image| image.status == STATUS_UPDATE)
            .count(),
        error_count: images
            .iter()
            .filter(|image| image.status == STATUS_ERROR)
            .count(),
        baseline_count: images
            .iter()
            .filter(|image| image.status == STATUS_BASELINE)
            .count(),
        has_last_run: last_run.is_some(),
        last_run_time: last_run
            .as_ref()
            .map(|run| format_timestamp(Some(run.started_at)))
            .unwrap_or_else(|| "never".to_string()),
        last_run_status: last_run
            .as_ref()
            .map(|run| run.status.clone())
            .unwrap_or_else(|| "never".to_string()),
        last_run_images: last_run.as_ref().map(|run| run.image_count).unwrap_or(0),
        last_run_errors: last_run.as_ref().map(|run| run.error_count).unwrap_or(0),
        has_images: !image_views.is_empty(),
        images: image_views,
        has_events: !recent_events.is_empty(),
        recent_events: recent_events
            .iter()
            .map(RecentEventView::from_record)
            .collect(),
    };
    let index_html = index.render().context("failed to render index template")?;

    fs::create_dir_all(&config.output_dir).with_context(|| {
        format!(
            "failed to create output directory {}",
            config.output_dir.display()
        )
    })?;
    fs::write(config.output_dir.join("index.html"), index_html).with_context(|| {
        format!(
            "failed to write {}",
            config.output_dir.join("index.html").display()
        )
    })?;
    fs::write(
        config.output_dir.join("style.css"),
        include_str!("../static/style.css"),
    )
    .with_context(|| {
        format!(
            "failed to write {}",
            config.output_dir.join("style.css").display()
        )
    })?;

    let images_dir = config.output_dir.join("images");
    fs::create_dir_all(&images_dir)
        .with_context(|| format!("failed to create output directory {}", images_dir.display()))?;

    for image in &images {
        let image_view = ImageView::from_record(image);
        let events = db::list_events(&connection, image.id, 100)?;
        let detail = DetailTemplate {
            css_path: "../style.css",
            home_path: "../index.html",
            generated_at: generated_at_display.clone(),
            has_events: !events.is_empty(),
            image: image_view,
            events: events.iter().map(EventView::from_record).collect(),
        };
        let html = detail
            .render()
            .with_context(|| format!("failed to render detail page for {}", image.reference))?;
        let path = images_dir.join(format!("{}.html", image.id));
        fs::write(&path, html).with_context(|| format!("failed to write {}", path.display()))?;
    }

    let status = StatusJson {
        generated_at: generated_at.timestamp(),
        last_run: last_run.as_ref().map(RunJson::from_record),
        images: images.iter().map(JsonImage::from_record).collect(),
    };
    let status_path = config.output_dir.join("status.json");
    fs::write(
        &status_path,
        serde_json::to_string_pretty(&status).context("failed to serialize status.json")?,
    )
    .with_context(|| format!("failed to write {}", status_path.display()))?;

    Ok(())
}

impl ImageView {
    fn from_record(image: &ImageRecord) -> Self {
        let (status_class, status_label) = status_display(&image.status);
        let digest_short = image
            .current_digest
            .as_deref()
            .map(short_digest)
            .unwrap_or_default();
        let compatible_tag = image.compatible_tag.clone().unwrap_or_default();
        let latest_tag = image.latest_tag.clone().unwrap_or_default();
        let has_latest = !latest_tag.is_empty() && latest_tag != compatible_tag;
        let mode_label = match image.mode.as_str() {
            db::MODE_SEMVER => "semver",
            db::MODE_LATEST => "latest only",
            db::MODE_DIGEST => "digest fallback",
            _ => "unknown",
        };

        Self {
            reference: image.reference.clone(),
            tag: image.tag.clone(),
            parsed_version: image.parsed_version.clone().unwrap_or_default(),
            has_parsed_version: image.parsed_version.is_some(),
            mode_label: mode_label.to_string(),
            status_class: status_class.to_string(),
            status_label: status_label.to_string(),
            has_compatible: !compatible_tag.is_empty(),
            compatible_tag,
            has_latest,
            latest_tag,
            has_digest: !digest_short.is_empty(),
            digest_short,
            has_error: image.last_error.is_some(),
            error: image.last_error.clone().unwrap_or_default(),
            first_seen: format_timestamp(Some(image.first_seen)),
            last_checked: format_timestamp(image.last_checked),
            last_changed: format_timestamp(image.last_changed),
            detail_url: format!("images/{}.html", image.id),
        }
    }
}

impl EventView {
    fn from_record(event: &EventRecord) -> Self {
        let old_value = event
            .old_value
            .as_deref()
            .map(|value| display_event_value(&event.kind, value))
            .unwrap_or_default();
        let new_value = event
            .new_value
            .as_deref()
            .map(|value| display_event_value(&event.kind, value))
            .unwrap_or_default();

        Self {
            kind_label: event_kind_label(&event.kind).to_string(),
            has_old: !old_value.is_empty(),
            has_new: !new_value.is_empty(),
            old_value,
            new_value,
            detected_at: format_timestamp(Some(event.detected_at)),
        }
    }
}

impl RecentEventView {
    fn from_record(event: &RecentEvent) -> Self {
        let old_value = event
            .old_value
            .as_deref()
            .map(|value| display_event_value(&event.kind, value))
            .unwrap_or_default();
        let new_value = event
            .new_value
            .as_deref()
            .map(|value| display_event_value(&event.kind, value))
            .unwrap_or_default();

        Self {
            reference: event.reference.clone(),
            kind_label: event_kind_label(&event.kind).to_string(),
            has_old: !old_value.is_empty(),
            has_new: !new_value.is_empty(),
            old_value,
            new_value,
            detected_at: format_timestamp(Some(event.detected_at)),
        }
    }
}

impl RunJson {
    fn from_record(run: &RunRecord) -> Self {
        Self {
            started_at: run.started_at,
            finished_at: run.finished_at,
            status: run.status.clone(),
            image_count: run.image_count,
            error_count: run.error_count,
        }
    }
}

impl JsonImage {
    fn from_record(image: &ImageRecord) -> Self {
        Self {
            id: image.id,
            reference: image.reference.clone(),
            registry: image.registry.clone(),
            repository: image.repository.clone(),
            tag: image.tag.clone(),
            mode: image.mode.clone(),
            parsed_version: image.parsed_version.clone(),
            status: image.status.clone(),
            current_digest: image.current_digest.clone(),
            baseline_digest: image.baseline_digest.clone(),
            compatible_tag: image.compatible_tag.clone(),
            latest_tag: image.latest_tag.clone(),
            last_checked: image.last_checked,
            last_changed: image.last_changed,
            last_error: image.last_error.clone(),
        }
    }
}

fn status_display(status: &str) -> (&'static str, &'static str) {
    match status {
        STATUS_UPDATE => ("update", "Update available"),
        STATUS_ERROR => ("error", "Error"),
        STATUS_BASELINE => ("baseline", "Baseline recorded"),
        _ => ("ok", "Up to date"),
    }
}

fn event_kind_label(kind: &str) -> &'static str {
    match kind {
        "compatible" => "compatible update",
        "latest" => "latest update",
        "digest" => "digest changed",
        _ => "change",
    }
}

fn display_event_value(kind: &str, value: &str) -> String {
    if kind == "digest" {
        short_digest(value)
    } else {
        value.to_string()
    }
}

fn short_digest(value: &str) -> String {
    if let Some(rest) = value.strip_prefix("sha256:") {
        let short: String = rest.chars().take(12).collect();
        format!("sha256:{short}")
    } else {
        value.chars().take(20).collect()
    }
}

fn format_timestamp(timestamp: Option<i64>) -> String {
    timestamp
        .and_then(|timestamp| DateTime::<Utc>::from_timestamp(timestamp, 0))
        .map(|date| date.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "never".to_string())
}
