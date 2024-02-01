use std::collections::HashSet;

use futures::{Future, FutureExt, Stream, StreamExt};
use jsonrpsee::{server::StopHandle, types::Params, RpcModule, SubscriptionMessage};
pub use rs_ts_api_macros::*;
use server::ServerService;
use ts_rs::Dependency;

pub mod server;

pub struct HandlerType {
    pub name: String,
    pub signature: String,
    pub dependencies: Vec<Dependency>,
}

pub trait Handler {
    fn register(rpc_builder: RpcBuilder) -> RpcBuilder;

    fn get_type() -> HandlerType;
}

pub struct RpcBuilder(RpcModule<()>);
impl RpcBuilder {
    pub fn new() -> Self {
        Self(RpcModule::new(()))
    }

    pub fn query<F, Fut>(mut self, name: &'static str, handler: F) -> Self
    where
        F: Fn(Params<'static>) -> Fut + Send + Sync + Clone + 'static,
        Fut: Future<Output = serde_json::Value> + Send + 'static,
    {
        self.0
            .register_async_method(name, move |params, _ctx| {
                let handler = handler.clone();

                async move {
                    handler(params).await;
                }
            })
            .unwrap();

        self
    }

    pub fn subscription<F, S>(
        mut self,
        name: &'static str,
        notification_name: &'static str,
        unsubscribe_name: &'static str,
        handler: F,
    ) -> Self
    where
        F: Fn(Params<'static>) -> S + Send + Sync + Clone + 'static,
        S: Stream<Item = serde_json::Value> + Send + 'static,
    {
        self.0
            .register_subscription(
                name,
                notification_name,
                unsubscribe_name,
                move |params, subscription, _ctx| {
                    let handler = handler.clone();

                    async move {
                        // Accept the subscription
                        let subscription = subscription.accept().await.unwrap();

                        // Set up a channel to avoid cloning the subscription
                        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

                        // Recieve values on a new thread, sending them onwards to the subscription
                        tokio::spawn(async move {
                            while let Some(value) = rx.recv().await {
                                subscription
                                    .send(SubscriptionMessage::from_json(&value).unwrap())
                                    .await
                                    .unwrap();
                            }
                        })
                        .await
                        .unwrap();

                        // Run the handler, capturing each of the values sand forwarding it onwards
                        // to the channel
                        handler(params)
                            .for_each(|value| tx.send(value).map(|result| result.unwrap()))
                            .await;
                    }
                },
            )
            .unwrap();

        self
    }
}

pub struct Router {
    name: Option<String>,
    handlers: Vec<fn() -> HandlerType>,
    rpc_builder: RpcBuilder,
}

impl Router {
    pub fn new() -> Self {
        Self {
            name: None,
            handlers: Vec::new(),
            rpc_builder: RpcBuilder::new(),
        }
    }

    pub fn namespace(name: impl ToString) -> Self {
        Self {
            name: Some(name.to_string()),
            handlers: Vec::new(),
            rpc_builder: RpcBuilder::new(),
        }
    }

    pub fn handler<H: Handler>(mut self, _: H) -> Self {
        self.rpc_builder = H::register(self.rpc_builder);
        self.handlers.push(H::get_type);

        self
    }

    pub fn get_type(&self) -> String {
        let (handlers, dependencies) = self
            .handlers
            .iter()
            .map(|get_type| get_type())
            .map(|handler_type| {
                (
                    format!("{}: {}", handler_type.name, handler_type.signature),
                    handler_type.dependencies,
                )
            })
            .unzip::<_, _, Vec<_>, Vec<_>>();

        // Generate the router type
        let mut router_type = format!("{{ {} }}", handlers.join(", "));

        // Merge all dependencies
        let dependencies = dependencies
            .into_iter()
            .flatten()
            .map(|dependency| {
                format!(
                    "import type {{ {} }} from \"./{}\";",
                    dependency.ts_name,
                    dependency.exported_to.trim_end_matches(".ts"),
                )
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();

        if let Some(name) = &self.name {
            router_type = format!("{{ {name}: {router_type} }}");
        }

        format!("{}\ntype Router = {router_type};", dependencies.join("\n"))
    }

    pub fn create_service(self, stop_handle: StopHandle) -> ServerService {
        let svc_builder = jsonrpsee::server::Server::builder().to_service_builder();

        ServerService {
            service: svc_builder.build(self.rpc_builder.0, stop_handle),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[allow(non_camel_case_types)]
    struct sample_handler;
    impl Handler for sample_handler {
        fn register(_rpc_builder: RpcBuilder) -> RpcBuilder {
            todo!()
        }

        fn get_type() -> HandlerType {
            HandlerType {
                name: "sample_handler".to_string(),
                signature: "() => void".to_string(),
                dependencies: Vec::new(),
            }
        }
    }

    #[allow(non_camel_case_types)]
    struct another_handler;
    impl Handler for another_handler {
        fn register(_rpc_builder: RpcBuilder) -> RpcBuilder {
            todo!()
        }

        fn get_type() -> HandlerType {
            HandlerType {
                name: "another_handler".to_string(),
                signature: "() => number".to_string(),
                dependencies: Vec::new(),
            }
        }
    }

    #[test]
    fn empty_router() {
        let router = Router::new();
        assert_eq!(router.get_type(), "{  }");
    }

    #[test]
    fn namespaced_empty_router() {
        let router = Router::namespace("ns");
        assert_eq!(router.get_type(), "{ ns: {  } }");
    }

    #[test]
    fn single_handler() {
        let router = Router::new().handler(sample_handler);
        assert_eq!(router.get_type(), "{ sample_handler: () => void }");
    }

    #[test]
    fn namespaced_single_handler() {
        let router = Router::namespace("ns").handler(sample_handler);
        assert_eq!(router.get_type(), "{ ns: { sample_handler: () => void } }");
    }

    #[test]
    fn multiple_handlers() {
        let router = Router::new()
            .handler(sample_handler)
            .handler(another_handler);
        assert_eq!(
            router.get_type(),
            "{ sample_handler: () => void, another_handler: () => void }"
        );
    }

    #[test]
    fn namespaced_multiple_handlers() {
        let router = Router::namespace("ns")
            .handler(sample_handler)
            .handler(another_handler);
        assert_eq!(
            router.get_type(),
            "{ ns: { sample_handler: () => void, another_handler: () => void } }"
        );
    }
}
