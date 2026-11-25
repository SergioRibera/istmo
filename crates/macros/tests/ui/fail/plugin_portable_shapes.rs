use std::time::Duration;

use istmo::FrameSinkId;

#[istmo::plugin(name = "ui.portable")]
pub trait Portable {
    async fn pair(&self, at: (u32, u32)) -> u32;
    async fn wait(&self, delay: Duration);
    async fn index(&self, i: usize) -> Vec<FrameSinkId<u32>>;
    async fn borrowed(&self, name: &'static str);
    async fn optional(&self, frames: Option<FrameSinkId<u32>>) -> Option<()>;
}

fn main() {}
