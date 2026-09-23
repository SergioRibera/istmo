use istmo::{message, plugin};

#[message]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EchoError {
    pub reason: String,
}

impl core::fmt::Display for EchoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for EchoError {}

#[plugin(name = "dev.istmo.demo.ios.echo")]
pub trait Echo {
    async fn echo(&self, text: String) -> Result<String, EchoError>;
}

#[derive(Debug, Default)]
pub struct EchoImpl;

impl Echo for EchoImpl {
    async fn echo(&self, text: String) -> Result<String, EchoError> {
        if text.is_empty() {
            return Err(EchoError {
                reason: "empty input".to_owned(),
            });
        }
        Ok(format!("echo: {text}"))
    }
}

istmo::runtime!(
    hosts: [
        Echo => EchoImpl,
    ],
);
