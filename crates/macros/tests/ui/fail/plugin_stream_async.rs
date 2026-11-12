#[istmo::plugin(name = "ui.stream_async")]
pub trait Ticker {
    #[istmo::stream]
    async fn ticks(&self) -> u32;
}

fn main() {}
