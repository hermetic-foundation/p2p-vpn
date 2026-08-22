use std::{
    fs,
    io::{BufRead as _, BufReader, Read as _, Write as _},
    os::unix::net::UnixListener,
    path::PathBuf,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use p2p_vpn::runtime::control_socket::{
    PairRpcCompletionArtifacts, PairRpcJoinStarted, PairRpcNixPlan, PairRpcOpenStarted,
    PairRpcOperationStatus, PairRpcPeer, PairRpcPhase, PairRpcReceipt, PairRpcRequest,
    PairRpcRequestEnvelope, PairRpcResponseEnvelope, PairRpcResult, PairRpcRole,
};

fn test_path(label: &str, extension: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "p2p-vpn-pair-cli-{}-{label}.{extension}",
        std::process::id()
    ))
}

fn pair_rpc_server(
    label: &str,
    handler: impl FnMut(PairRpcRequestEnvelope) -> PairRpcResponseEnvelope + Send + 'static,
) -> (PathBuf, thread::JoinHandle<()>) {
    pair_rpc_server_requests(label, 1, handler)
}

fn pair_rpc_server_requests(
    label: &str,
    request_count: usize,
    mut handler: impl FnMut(PairRpcRequestEnvelope) -> PairRpcResponseEnvelope + Send + 'static,
) -> (PathBuf, thread::JoinHandle<()>) {
    let socket = test_path(label, "sock");
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).expect("bind pair RPC socket");
    listener
        .set_nonblocking(true)
        .expect("set pair RPC listener nonblocking");
    let server = thread::spawn(move || {
        for _ in 0..request_count {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "pair RPC client did not connect");
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept pair RPC client: {error}"),
                }
            };
            let mut header = String::new();
            let mut reader = BufReader::new(&mut stream);
            reader.read_line(&mut header).expect("read pair RPC header");
            let body_len = header
                .strip_prefix("rpc-v1 ")
                .and_then(|header| header.trim_end().parse::<usize>().ok())
                .expect("valid pair RPC header");
            let mut body = vec![0; body_len];
            reader.read_exact(&mut body).expect("read pair RPC body");
            drop(reader);
            let request = serde_json::from_slice(&body).expect("decode pair RPC request");
            let response = serde_json::to_vec(&handler(request)).expect("encode pair RPC response");
            write!(stream, "rpc-v1 {}\n", response.len()).expect("write pair RPC response header");
            stream
                .write_all(&response)
                .expect("write pair RPC response body");
        }
    });
    (socket, server)
}

#[test]
fn pair_open_cli_round_trips_running_daemon_rpc() {
    let (socket, server) = pair_rpc_server("open", |request| {
        let PairRpcRequest::PairOpen {
            operation_id,
            expires_in_seconds,
        } = request.request
        else {
            panic!("expected pair open request");
        };
        assert_eq!(expires_in_seconds, 120);
        PairRpcResponseEnvelope::ok(PairRpcResult::OpenStarted(PairRpcOpenStarted {
            operation_id,
            code: "ABCD-EFGH-JKLM-NPQR".to_owned(),
            network_name: "runner-mesh".to_owned(),
            local_peer: "12D3KooWLocal".to_owned(),
            expires_at_unix_seconds: 1_700_000_120,
        }))
    });

    let output = Command::new(env!("CARGO_BIN_EXE_p2p-vpn"))
        .args([
            "pair",
            "open",
            "--socket",
            socket.to_str().expect("socket path"),
            "--expires-in-seconds",
            "120",
            "--format",
            "json",
        ])
        .output()
        .expect("run pair open");

    server.join().expect("pair RPC server");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("open JSON");
    assert_eq!(result["code"], "ABCD-EFGH-JKLM-NPQR");
    assert_eq!(result["network_name"], "runner-mesh");
    let _ = fs::remove_file(socket);
}

#[test]
fn pair_join_cli_waits_for_daemon_completion() {
    let mut operation_id = None;
    let mut status_requests = 0;
    let (socket, server) =
        pair_rpc_server_requests("join", 3, move |request| match request.request {
            PairRpcRequest::PairJoin {
                operation_id: requested_operation,
                code,
                timeout_seconds,
                ..
            } => {
                assert_eq!(code, "ABCD-EFGH-JKLM-NPQR");
                assert_eq!(timeout_seconds, 10);
                operation_id = Some(requested_operation.clone());
                PairRpcResponseEnvelope::ok(PairRpcResult::JoinStarted(PairRpcJoinStarted {
                    operation_id: requested_operation,
                    network_name: "runner-mesh".to_owned(),
                    local_peer: "12D3KooWLocal".to_owned(),
                    expires_at_unix_seconds: 1_700_000_010,
                }))
            }
            PairRpcRequest::PairStatus {
                operation_id: requested_operation,
            } => {
                assert_eq!(Some(&requested_operation), operation_id.as_ref());
                status_requests += 1;
                let phase = if status_requests == 1 {
                    PairRpcPhase::AwaitingApproval
                } else {
                    PairRpcPhase::Completed
                };
                PairRpcResponseEnvelope::ok(PairRpcResult::OperationStatus(Box::new(
                    PairRpcOperationStatus {
                        operation_id: requested_operation,
                        network_name: "runner-mesh".to_owned(),
                        local_peer: "12D3KooWLocal".to_owned(),
                        role: PairRpcRole::Joiner,
                        phase,
                        revision: u64::try_from(status_requests).expect("status revision"),
                        discovery: None,
                        expires_at_unix_seconds: 1_700_000_010,
                        candidate: None,
                        artifacts_ready: phase == PairRpcPhase::Completed,
                        failure: None,
                    },
                )))
            }
            request => panic!("unexpected pair join RPC: {request:?}"),
        });

    let output = Command::new(env!("CARGO_BIN_EXE_p2p-vpn"))
        .args([
            "pair",
            "join",
            "ABCD-EFGH-JKLM-NPQR",
            "--socket",
            socket.to_str().expect("socket path"),
            "--timeout-seconds",
            "10",
            "--format",
            "json",
        ])
        .output()
        .expect("run pair join");

    server.join().expect("pair RPC server");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("completed status JSON");
    assert_eq!(status["phase"], "completed");
    assert_eq!(status["artifacts_ready"], true);
    let _ = fs::remove_file(socket);
}

#[test]
fn pair_artifacts_cli_writes_native_nix_without_json_or_secrets() {
    let operation_id = "pair-operation";
    let artifacts = PairRpcCompletionArtifacts {
        receipt: PairRpcReceipt {
            network_name: "runner-mesh".to_owned(),
            local_peer: "12D3KooWLocal".to_owned(),
            remote_peer: "12D3KooWRemote".to_owned(),
            role: PairRpcRole::Joiner,
            transcript_sha256: "transcript-digest".to_owned(),
            completed_at_unix_seconds: 1_700_000_100,
        },
        nix: PairRpcNixPlan {
            instance_name: "runner-mesh".to_owned(),
            network_name: "runner-mesh".to_owned(),
            local_peer: "12D3KooWLocal".to_owned(),
            assigned_vpn_ip: Some("10.42.0.2".to_owned()),
            additional_local_routes: Vec::new(),
            peer: PairRpcPeer {
                id: "12D3KooWRemote".to_owned(),
                name: None,
                vpn_ip: None,
                routes: Vec::new(),
            },
            member_records: Vec::new(),
            membership_key_file: None,
        },
    };
    let (socket, server) = pair_rpc_server("artifacts", move |request| {
        assert_eq!(
            request.request,
            PairRpcRequest::PairArtifacts {
                operation_id: operation_id.to_owned(),
            }
        );
        PairRpcResponseEnvelope::ok(PairRpcResult::Artifacts(Box::new(artifacts.clone())))
    });
    let output_path = test_path("artifacts", "nix");
    let _ = fs::remove_file(&output_path);

    let output = Command::new(env!("CARGO_BIN_EXE_p2p-vpn"))
        .args([
            "pair",
            "artifacts",
            operation_id,
            "--socket",
            socket.to_str().expect("socket path"),
            "--output",
            output_path.to_str().expect("output path"),
        ])
        .output()
        .expect("run pair artifacts");

    server.join().expect("pair RPC server");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rendered = fs::read_to_string(&output_path).expect("native Nix artifact");
    assert!(rendered.contains("services.p2p-vpn.instances.\"runner-mesh\""));
    assert!(rendered.contains("localPeer = \"12D3KooWLocal\";"));
    assert!(rendered.contains("\"12D3KooWRemote\" = {"));
    assert!(!rendered.contains("private_key"));
    assert!(!rendered.contains("membership_key"));
    assert!(serde_json::from_str::<serde_json::Value>(&rendered).is_err());
    let _ = fs::remove_file(socket);
    let _ = fs::remove_file(output_path);
}
