use std::sync::Arc;

use aw_client_rust::AwClient;
use chrono::{DateTime, Local, TimeDelta};
use clap::{value_parser, Arg, Command};
use serde_json::{Map, Value};
use tokio::sync::Mutex;
use tower_lsp::{jsonrpc, lsp_types::*, Client, LanguageServer, LspService, Server};

#[derive(Default, Debug)]
struct Event {
    uri: String,
    is_write: bool,
    language: Option<String>,
}

#[derive(Debug)]
struct CurrentFile {
    uri: String,
    timestamp: DateTime<Local>,
    // TODO: seems we're gonna have to track each open files language bleh. Doublecheck wakatime impl
}

struct ActivityWatchLangaugeServer {
    client: Client,
    current_file: Mutex<CurrentFile>,
    aw_client: AwClient,
    // TODO: consider moving?
    bucket_id: String,
}

impl ActivityWatchLangaugeServer {
    async fn send(&self, event: Event) {
        // if isWrite is false, and file has not changed since last heartbeat,
        // and it has been fewer than 2 minutes since last heartbeat do nothing
        const INTERVAL: TimeDelta = TimeDelta::minutes(2);

        let mut current_file = self.current_file.lock().await;
        let now = Local::now();

        if event.uri == current_file.uri
            && now - current_file.timestamp < INTERVAL
            && event.is_write
        {
            return;
        }

        let mut data = Map::new();
        data.insert("file".to_string(), Value::String(event.uri.clone()));
        match self.client.workspace_folders().await {
            Ok(o) => {
                if let Some(folders) = o {
                    // ActivityWatch's API only lets us report the first folder. I think Zed only ever reports one anyway
                    if let Some(folder) = folders.first() {
                        data.insert(
                            "project".to_string(),
                            Value::String(String::from(folder.uri.clone())),
                        );
                    }
                };
            }
            Err(_) => todo!(),
        };
        if let Some(language) = event.language {
            data.insert("language".to_string(), Value::String(language));
        }

        // Duration 0 bc heartbeats?? https://docs.activitywatch.net/en/latest/buckets-and-events.html#id7
        // https://github.com/ActivityWatch/aw-watcher-vscode/blob/36093d4ac133f04363f144bdfefa4523f8e8f25f/src/extension.ts#L139
        let aw_event = aw_client_rust::Event::new(now.to_utc(), TimeDelta::zero(), data);

        const PULSETIME: f64 = (INTERVAL.num_seconds() - 10) as f64;
        self.aw_client
            // TODO: double check interval stuff
            .heartbeat(&self.bucket_id, &aw_event, PULSETIME)
            .await
            .unwrap();

        //let settings = self.settings.load();

        current_file.uri = event.uri;
        current_file.timestamp = now;
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for ActivityWatchLangaugeServer {
    async fn initialize(&self, _: InitializeParams) -> jsonrpc::Result<InitializeResult> {
        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: env!("CARGO_PKG_NAME").to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                ..Default::default()
            },
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        self.client
            .log_message(
                MessageType::INFO,
                "ActivityWatch language server initialized",
            )
            .await;
    }

    async fn shutdown(&self) -> jsonrpc::Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let event = Event {
            uri: params.text_document.uri[url::Position::BeforeUsername..].to_string(),
            is_write: false,
            language: Some(params.text_document.language_id.clone()),
        };

        self.send(event).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let event = Event {
            uri: params.text_document.uri[url::Position::BeforeUsername..].to_string(),
            is_write: false,
            language: None,
        };

        self.send(event).await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let event = Event {
            uri: params.text_document.uri[url::Position::BeforeUsername..].to_string(),
            is_write: true,
            language: None,
        };

        self.send(event).await;
    }
}

#[tokio::main]
async fn main() {
    let matches = Command::new("activitywatch_ls")
        .version(env!("CARGO_PKG_VERSION"))
        .author("Sacha Korban <sk@sachk.com>")
        .about("A simple ActivityWatch language server watcher")
        .arg(
            Arg::new("host")
                .short('a')
                .long("host")
                .help("The host of the ActivityWatch server to connect to")
                .required(false)
                .default_value("localhost"),
        )
        .arg(
            Arg::new("port")
                .value_parser(value_parser!(u16))
                .short('p')
                .long("port")
                .help("The ActivityWatch server port to connect to on the host")
                .required(false)
                // TODO: change to 5600
                .default_value("5666"),
        )
        .get_matches();

    // TODO: clean up and handle errors
    // Note that AwClient does not support https
    //
    // this kinda sucks doesn't it??? is there a nicer way to do this or is it better to just not use default_value
    let host: &String = matches.get_one("host").unwrap();
    let port: &u16 = matches.get_one("port").unwrap();
    println!("got host {host} and port {port}");

    let aw_client = AwClient::new(host, *port, "aw-watcher-zed").unwrap();

    let bucket_id = format!("test-client-bucket_{}", aw_client.hostname);
    // TODO: check if we should be checking for a preexisting bucket?
    aw_client
        .create_bucket_simple(&bucket_id, "app.editor.activity")
        .await
        .unwrap();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(|client| {
        Arc::new(ActivityWatchLangaugeServer {
            client,
            current_file: Mutex::new(CurrentFile {
                uri: String::new(),
                timestamp: Local::now(),
            }),
            aw_client,
            bucket_id,
        })
    });

    Server::new(stdin, stdout, socket).serve(service).await;
}
