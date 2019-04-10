use crate::errors::Error;

use futures::compat::Future01CompatExt;
use futures::{FutureExt, Stream};
use std::future::Future;
use std::pin::Pin;
use std::task::{Poll, Waker};
use std::time::Duration;

type Outstanding<T> = Pin<Box<Future<Output=Result<(Option<T>, crossbeam_channel::Receiver<T>), Error>> + Send>>;

enum ReceiveState<T> {
    None,
    Ready(crossbeam_channel::Receiver<T>),
    Pending(Outstanding<T>),
}

pub struct Receiver<T> {
    inner: ReceiveState<T>,
    delay: Duration,
}

impl<T> Receiver<T> {
    pub fn new(r: crossbeam_channel::Receiver<T>, delay: Duration) -> Receiver<T> {
        Receiver {
            inner: ReceiveState::Ready(r),
            delay: delay,
        }
    }

    fn delay(&self) -> Duration { self.delay }

    fn inner<'a>(self: Pin<&'a mut Self>) -> &'a mut ReceiveState<T> {
        unsafe {
            &mut Pin::get_unchecked_mut(self).inner
        }
    }
}

async fn receive<T>(receiver: crossbeam_channel::Receiver<T>, delay: Duration) -> Result<(Option<T>, crossbeam_channel::Receiver<T>), Error> {
    loop {
        match receiver.try_recv() {
            Err(crossbeam_channel::TryRecvError::Disconnected) => return Ok((None, receiver)),
            Err(crossbeam_channel::TryRecvError::Empty) => {
                await!(tokio_timer::sleep(delay).compat()).map_err(Error::TokioTimer)?
            }
            Ok(v) => return Ok( (Some(v), receiver) ),
        }
    }
}

impl<T: Send + 'static> Stream for Receiver<T> {
    type Item = Result<T, Error>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        waker: &Waker,
    ) -> Poll<Option<Self::Item>> {
        loop {
            let inner = std::mem::replace(self.as_mut().inner(), ReceiveState::None);
            match inner {
                ReceiveState::None => panic!("Cannot call poll_next twice"),
                ReceiveState::Ready(r) => {
                    let delay = self.delay;
                    let fut = receive(r, delay);
                    *self.as_mut().inner() = ReceiveState::Pending(fut.boxed());
                }
                ReceiveState::Pending(mut f) => {
                    match f.as_mut().poll(waker) {
                        Poll::Pending => {
                            *self.as_mut().inner() = ReceiveState::Pending(f);
                            return Poll::Pending;
                        }
                        Poll::Ready(Err(e)) => {
                            return Poll::Ready(Some(Err(e)));
                        }
                        Poll::Ready( Ok( (opt_v, r) ) ) => {
                            *self.as_mut().inner() = ReceiveState::Ready(r);
                            return Poll::Ready(opt_v.map(|v| Ok(v)));
                        }
                    }
                }
            }
        }
    }
}