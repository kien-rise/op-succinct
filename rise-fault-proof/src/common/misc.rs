use futures::future::select;
use std::{future::Future, pin::pin};

pub use futures::future::Either;

pub async fn select2<F1: Future, F2: Future>(f1: F1, f2: F2) -> Either<F1::Output, F2::Output> {
    match select(pin!(f1), pin!(f2)).await {
        Either::Left((out1, _)) => Either::Left(out1),
        Either::Right((out2, _)) => Either::Right(out2),
    }
}
