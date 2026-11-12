#[istmo::plugin(name = "ui.generic_method")]
pub trait Convert {
    async fn convert<T>(&self, value: u32) -> u32;
}

fn main() {}
