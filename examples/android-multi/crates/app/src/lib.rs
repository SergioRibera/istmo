use std::thread;
use std::time::Duration;

use istmo::plugins::{NotificationSpec, ServiceContext};
use istmo::{IstmoError, message, plugin, service};

#[message]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiError {
    pub reason: String,
}

impl core::fmt::Display for MultiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for MultiError {}

fn to_multi_error(err: &IstmoError) -> MultiError {
    MultiError {
        reason: err.to_string(),
    }
}

#[plugin(name = "dev.istmo.multi.app_control")]
pub trait AppControl {
    async fn ping(&self) -> Result<String, MultiError>;
}

#[derive(Debug, Default)]
pub struct AppControlImpl;

impl AppControl for AppControlImpl {
    async fn ping(&self) -> Result<String, MultiError> {
        Ok("pong from Rust".to_owned())
    }
}

#[service(name = "dev.istmo.multi.sync")]
pub trait SyncService {
    async fn on_start(&self, ctx: ServiceContext) -> Result<(), MultiError>;
    async fn on_stop(&self);
}

#[derive(Debug, Default)]
pub struct SyncImpl;

const NOTIFICATION_CHANNEL: &str = "istmo-multi-sync";
const NOTIFICATION_ID: i32 = 42;

const FOREGROUND_TYPE_DATA_SYNC: u32 = 0x0000_0001;

impl SyncService for SyncImpl {
    async fn on_start(&self, ctx: ServiceContext) -> Result<(), MultiError> {
        let spec = NotificationSpec {
            channel_id: NOTIFICATION_CHANNEL.to_owned(),
            notification_id: NOTIFICATION_ID,
            title: "istmo multi".to_owned(),
            body: "Sync running".to_owned(),
            small_icon: None,
            ongoing: true,
            foreground_service_type: Some(FOREGROUND_TYPE_DATA_SYNC),
        };
        ctx.set_foreground(spec)
            .await
            .map_err(|e| to_multi_error(&e))?;

        let mut ticks: u32 = 0;
        while !ctx.is_stopped() {
            ticks = ticks.saturating_add(1);
            let update = NotificationSpec {
                channel_id: NOTIFICATION_CHANNEL.to_owned(),
                notification_id: NOTIFICATION_ID,
                title: "istmo multi".to_owned(),
                body: format!("tick {ticks}"),
                small_icon: None,
                ongoing: true,
                foreground_service_type: Some(FOREGROUND_TYPE_DATA_SYNC),
            };

            drop(ctx.update_notification(update).await);
            thread::sleep(Duration::from_secs(2));
        }
        Ok(())
    }

    async fn on_stop(&self) {}
}

istmo::runtime!(
    hosts: [
        AppControl => AppControlImpl,
    ],
    services: [
        SyncService => SyncImpl,
    ],
);

