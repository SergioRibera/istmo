#[istmo::message]
#[derive(Debug)]
pub struct Oops;

#[istmo::plugin(name = "ui.error_impl")]
pub trait Fallible {
    async fn run(&self) -> Result<u32, Oops>;
}

fn main() {}
