use std::future::Future;

pub use thin_cancellation_token::CancellationToken as Thin;
pub use tokio_util::sync::CancellationToken as Tokio;

pub trait Token: Clone + Send + Sync + 'static {
    const NAME: &'static str;
    fn new() -> Self;
    fn cancel(&self);
    fn is_cancelled(&self) -> bool;
    fn cancelled(&self) -> impl Future<Output = ()> + Send;
}

macro_rules! token {
    ($ty:ty, $name:literal) => {
        impl Token for $ty {
            const NAME: &'static str = $name;
            fn new() -> Self {
                <$ty>::new()
            }
            fn cancel(&self) {
                <$ty>::cancel(self);
            }
            fn is_cancelled(&self) -> bool {
                <$ty>::is_cancelled(self)
            }
            fn cancelled(&self) -> impl Future<Output = ()> + Send {
                <$ty>::cancelled(self)
            }
        }
    };
}
token!(Thin, "thin");
token!(Tokio, "tokio_util");
