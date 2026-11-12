#[istmo::plugin(name = "ui.not_async")]
pub trait Clock {
    fn now(&self) -> u64;
}

fn main() {}
