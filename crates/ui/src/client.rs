//! The connection to the daemon, on its own thread.

use std::time::Duration;

use tern_ipc::{Event, Request, Response};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum News {
    Event(Event),
    /// The daemon is not there (any more); the cache still is.
    Gone,
    /// Asking for older messages ended; `true` when it worked.
    Older {
        conversation: String,
        done: bool,
    },
    /// The daemon did not take a message or an edit; what was written must not be lost.
    Unsent {
        request: Box<Request>,
        reason: String,
    },
    /// A request was refused.
    Refused(String),
}

#[derive(Clone)]
pub struct Client {
    requests: mpsc::UnboundedSender<Request>,
}

const RETRY: Duration = Duration::from_secs(2);
/// Some requests go to the service and back.
const PATIENCE: Duration = Duration::from_secs(60);

impl Client {
    pub fn start() -> (Client, mpsc::UnboundedReceiver<News>) {
        let (requests, queue) = mpsc::unbounded_channel();
        let (news, received) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            if let Ok(runtime) = runtime {
                tokio::task::LocalSet::new().block_on(&runtime, run(queue, news));
            }
        });
        (Client { requests }, received)
    }

    /// Nobody answers; for the demo.
    pub fn nowhere() -> Client {
        Client {
            requests: mpsc::unbounded_channel().0,
        }
    }

    pub fn ask(&self, request: Request) {
        let _ = self.requests.send(request);
    }
}

async fn run(mut queue: mpsc::UnboundedReceiver<Request>, news: mpsc::UnboundedSender<News>) {
    let follow = async {
        loop {
            let _ = tern_ipc::watch(|event| drop(news.send(News::Event(event)))).await;
            if news.send(News::Gone).is_err() {
                return;
            }
            tokio::time::sleep(RETRY).await;
        }
    };
    let ask = async {
        while let Some(request) = queue.recv().await {
            let news = news.clone();
            if matches!(request, Request::Send { .. }) {
                // In the order they were written.
                answer(request, news).await;
            } else {
                tokio::task::spawn_local(answer(request, news));
            }
        }
    };
    tokio::select! {
        () = follow => {}
        () = ask => {}
    }
}

async fn answer(request: Request, news: mpsc::UnboundedSender<News>) {
    let outcome = tern_ipc::ask_within(&request, PATIENCE).await;
    if let Request::Older { conversation } = request {
        let done = matches!(outcome, Ok(Response::Done));
        let _ = news.send(News::Older { conversation, done });
        return;
    }
    let refusal = match outcome {
        Ok(Response::Error(error)) => error,
        Ok(_) => {
            if let Request::Send { files, .. } = &request {
                crate::composer::forget_pasted(files);
            }
            return;
        }
        Err(_) => "the background service is not running".to_owned(),
    };
    if matches!(request, Request::Send { .. } | Request::Edit { .. }) {
        let _ = news.send(News::Unsent {
            request: Box::new(request),
            reason: refusal,
        });
        return;
    }
    // Nobody asked for these in so many words.
    if !matches!(
        request,
        Request::MarkRead { .. } | Request::Look { .. } | Request::Typing { .. }
    ) {
        let _ = news.send(News::Refused(refusal));
    }
}
