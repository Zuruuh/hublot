use std::time::Duration;

use anyhow::{Context, Result};
use lettre::{
    message::{header::ContentType, Mailbox},
    transport::smtp::{
        authentication::Credentials,
        client::{Tls, TlsParameters},
    },
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use tracing::info;

use crate::{
    config::{Config, SmtpConfig, SmtpTls},
    db::{self, ImageRecord},
};

struct Change {
    image_id: i64,
    reference: String,
    parsed_version: Option<String>,
    compatible: Option<String>,
    latest: Option<String>,
    digest: Option<String>,
    compatible_changed: bool,
    latest_changed: bool,
    digest_changed: bool,
}

pub async fn send_pending(config: &Config) -> Result<usize> {
    let Some(smtp) = &config.smtp else {
        return Ok(0);
    };

    let connection = db::open(&config.database)?;
    db::init(&connection)?;
    let images = db::list_active_images(&connection)?;
    let changes: Vec<Change> = images.iter().filter_map(Change::from_record).collect();

    if changes.is_empty() {
        info!("no unreported image updates; skipping email");
        return Ok(0);
    }

    let count = changes.len();
    let subject = format!(
        "hublot: {count} image update{}",
        if count == 1 { "" } else { "s" }
    );
    send_email(smtp, &subject, &render_body(&changes)).await?;

    let transaction = connection.unchecked_transaction()?;
    for change in &changes {
        db::mark_notified(
            &transaction,
            change.image_id,
            change.compatible.as_deref(),
            change.latest.as_deref(),
            change.digest.as_deref(),
        )?;
    }
    transaction.commit()?;

    info!(count, "sent update email");
    Ok(count)
}

impl Change {
    fn from_record(image: &ImageRecord) -> Option<Self> {
        match image.mode.as_str() {
            db::MODE_SEMVER | db::MODE_LATEST => {
                let compatible_changed = image.compatible_tag.is_some()
                    && image.compatible_tag != image.notified_compatible_tag;
                let latest_changed =
                    image.latest_tag.is_some() && image.latest_tag != image.notified_latest_tag;

                if !compatible_changed && !latest_changed {
                    return None;
                }

                Some(Self {
                    image_id: image.id,
                    reference: image.reference.clone(),
                    parsed_version: image.parsed_version.clone(),
                    compatible: image.compatible_tag.clone(),
                    latest: image.latest_tag.clone(),
                    digest: None,
                    compatible_changed,
                    latest_changed,
                    digest_changed: false,
                })
            }
            db::MODE_DIGEST => {
                if image.current_digest.is_none() || image.current_digest == image.notified_digest {
                    return None;
                }

                Some(Self {
                    image_id: image.id,
                    reference: image.reference.clone(),
                    parsed_version: None,
                    compatible: None,
                    latest: None,
                    digest: image.current_digest.clone(),
                    compatible_changed: false,
                    latest_changed: false,
                    digest_changed: true,
                })
            }
            _ => None,
        }
    }
}

fn render_body(changes: &[Change]) -> String {
    let mut body = String::from("hublot found updates:\n");

    for change in changes {
        body.push('\n');
        body.push_str(&change.reference);
        body.push('\n');

        if let Some(parsed_version) = &change.parsed_version {
            body.push_str("  parsed version: ");
            body.push_str(parsed_version);
            body.push('\n');
        }

        if change.compatible_changed {
            if let Some(compatible) = &change.compatible {
                body.push_str("  compatible: ");
                body.push_str(compatible);
                body.push('\n');
            }
        }

        if change.latest_changed {
            if let Some(latest) = &change.latest {
                if change.compatible.as_ref() != Some(latest) {
                    body.push_str("  latest: ");
                    body.push_str(latest);
                    body.push('\n');
                }
            }
        }

        if change.digest_changed {
            if let Some(digest) = &change.digest {
                body.push_str("  digest: ");
                body.push_str(digest);
                body.push('\n');
            }
        }
    }

    body
}

async fn send_email(smtp: &SmtpConfig, subject: &str, body: &str) -> Result<()> {
    let from: Mailbox = smtp
        .from
        .parse()
        .with_context(|| format!("invalid smtp.from address {}", smtp.from))?;

    let mut builder = Message::builder()
        .from(from)
        .subject(subject)
        .header(ContentType::TEXT_PLAIN);

    for recipient in &smtp.to {
        let mailbox: Mailbox = recipient
            .parse()
            .with_context(|| format!("invalid smtp.to address {recipient}"))?;
        builder = builder.to(mailbox);
    }

    let message = builder
        .body(body.to_string())
        .context("failed to build email message")?;

    build_transport(smtp)?
        .send(message)
        .await
        .context("failed to send email")?;

    Ok(())
}

fn build_transport(smtp: &SmtpConfig) -> Result<AsyncSmtpTransport<Tokio1Executor>> {
    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(smtp.host.clone())
        .port(smtp.port)
        .timeout(Some(Duration::from_secs(smtp.timeout_secs)));

    builder = match smtp.tls_mode() {
        SmtpTls::Implicit => {
            let parameters = TlsParameters::new(smtp.host.clone())
                .context("failed to build SMTP TLS parameters")?;
            builder.tls(Tls::Wrapper(parameters))
        }
        SmtpTls::Starttls => {
            let parameters = TlsParameters::new(smtp.host.clone())
                .context("failed to build SMTP TLS parameters")?;
            builder.tls(Tls::Required(parameters))
        }
        SmtpTls::None => builder.tls(Tls::None),
    };

    if let Some((username, password)) = smtp.credentials()? {
        builder = builder.credentials(Credentials::new(username, password));
    }

    Ok(builder.build())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(mode: &str) -> ImageRecord {
        ImageRecord {
            id: 1,
            reference: "docker.io/library/nginx:1.27.3".to_string(),
            registry: "docker.io".to_string(),
            repository: "library/nginx".to_string(),
            tag: "1.27.3".to_string(),
            mode: mode.to_string(),
            parsed_version: Some("1.27.3".to_string()),
            current_digest: None,
            baseline_digest: None,
            notified_compatible_tag: None,
            notified_latest_tag: None,
            notified_digest: None,
            compatible_tag: Some("1.31.8".to_string()),
            latest_tag: Some("2.0.0".to_string()),
            status: db::STATUS_UPDATE.to_string(),
            last_error: None,
            active: true,
            first_seen: 0,
            last_checked: None,
            last_changed: None,
        }
    }

    #[test]
    fn skips_previously_reported_updates() {
        let mut record = image(db::MODE_SEMVER);
        record.notified_compatible_tag = record.compatible_tag.clone();
        record.notified_latest_tag = record.latest_tag.clone();
        assert!(Change::from_record(&record).is_none());
    }

    #[test]
    fn reports_only_changed_candidates() {
        let mut record = image(db::MODE_SEMVER);
        record.notified_compatible_tag = Some("1.30.0".to_string());
        record.notified_latest_tag = record.latest_tag.clone();

        let change = Change::from_record(&record).unwrap();
        assert!(change.compatible_changed);
        assert!(!change.latest_changed);
        assert_eq!(change.compatible.as_deref(), Some("1.31.8"));
        assert_eq!(change.latest.as_deref(), Some("2.0.0"));
    }

    #[test]
    fn hides_duplicate_latest_in_body() {
        let mut record = image(db::MODE_SEMVER);
        record.compatible_tag = Some("2.0.0".to_string());
        record.latest_tag = Some("2.0.0".to_string());
        let change = Change::from_record(&record).unwrap();
        let body = render_body(&[change]);
        assert!(body.contains("compatible: 2.0.0"));
        assert!(!body.contains("latest: 2.0.0"));
    }

    #[test]
    fn builds_all_tls_modes() {
        let base = SmtpConfig {
            host: "smtp.example.com".to_string(),
            port: 465,
            username: None,
            password_file: None,
            from: "hublot@example.com".to_string(),
            to: vec!["me@example.com".to_string()],
            tls: None,
            timeout_secs: 30,
        };

        for tls in [SmtpTls::Implicit, SmtpTls::Starttls, SmtpTls::None] {
            let mut smtp = base.clone();
            smtp.tls = Some(tls);
            assert!(build_transport(&smtp).is_ok());
        }
    }

    #[test]
    fn reports_digest_changes() {
        let mut record = image(db::MODE_DIGEST);
        record.current_digest = Some("sha256:new".to_string());
        record.notified_digest = Some("sha256:old".to_string());
        let change = Change::from_record(&record).unwrap();
        assert_eq!(change.digest.as_deref(), Some("sha256:new"));
    }
}
