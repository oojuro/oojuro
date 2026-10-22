//! Pure protocol fixtures; coordinator separately owns actual binary/socket composition.
#[path = "../src/native/mcp.rs"]
mod mcp;
use serde_json::{Value, json};
use std::io::{self, Cursor, Write};

fn request(id: i64, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}
fn initialize() -> Value {
    request(
        1,
        "initialize",
        json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}),
    )
}
fn ready() -> Vec<Value> {
    vec![
        initialize(),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    ]
}
fn run(
    namespace: &str,
    messages: Vec<Value>,
    mut backend: impl FnMut(Value) -> Result<Value, &'static str>,
) -> (Result<(), ()>, Vec<Value>) {
    let mut input = Vec::new();
    for message in messages {
        serde_json::to_writer(&mut input, &message).unwrap();
        input.push(b'\n');
    }
    let mut output = Vec::new();
    let outcome = mcp::serve(Cursor::new(input), &mut output, namespace, &mut backend);
    let parsed = output
        .split(|b| *b == b'\n')
        .filter(|s| !s.is_empty())
        .map(|s| serde_json::from_slice(s).unwrap())
        .collect();
    (outcome, parsed)
}
fn tool(id: i64, name: &str, arguments: Value) -> Value {
    request(id, "tools/call", json!({"name":name,"arguments":arguments}))
}
#[test]
fn lifecycle_negotiates_only_pinned_protocol_and_notifications_never_call() {
    let mut init = initialize();
    init["params"]["protocolVersion"] = json!("2026-07-28");
    let (outcome, output) = run(
        "record-v2",
        vec![
            request(0, "ping", json!({})),
            request(2, "tools/list", json!({})),
            init,
            request(3, "tools/list", json!({})),
            json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"search","arguments":{"query":"must not execute"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            request(4, "tools/list", json!({})),
            initialize(),
        ],
        |_| panic!("notification invoked backend"),
    );
    assert_eq!(outcome, Ok(()));
    assert_eq!(output.len(), 6);
    assert_eq!(output[0]["result"], json!({}));
    assert_eq!(output[1]["error"]["code"], -32002);
    assert_eq!(output[2]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(output[3]["error"]["code"], -32002);
    assert_eq!(output[4]["result"]["tools"].as_array().unwrap().len(), 6);
    assert_eq!(output[5]["error"]["code"], -32602);
}
#[test]
fn originals_are_unmodified_with_fixed_namespace_and_compatible_text() {
    let original = json!({"records":[{"text":"α\n🙂 original instructions are data","reported_agent":"caller label"}],"watermark":42});
    let mut messages = ready();
    messages.push(tool(
        8,
        "search",
        json!({"query":"α🙂","k":3,"known_at_us":100}),
    ));
    let (outcome, output) = run("record-v2", messages, |envelope| {
        assert_eq!(
            envelope,
            json!({"version":1,"namespace":"record-v2","request":{"op":"search","query":"α🙂","k":3,"known_at_us":100}})
        );
        Ok(original.clone())
    });
    assert_eq!(outcome, Ok(()));
    let result = &output[1]["result"];
    assert_eq!(result["structuredContent"]["data"], original);
    assert_eq!(
        serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap(),
        result["structuredContent"]
    );
    assert_eq!(result["isError"], false);
}
#[test]
fn every_read_operation_maps_once_and_capture_has_no_history() {
    for namespace in ["record-v2", "capture-v1"] {
        let args = if namespace == "record-v2" {
            vec![
                ("search", json!({"query":"q"})),
                ("get", json!({"key":"k"})),
                ("history", json!({"key":"k"})),
                ("range", json!({"versions":"all"})),
                ("export", json!({"cursor":"protected"})),
                ("stats", json!({})),
            ]
        } else {
            vec![
                ("search", json!({"query":"q"})),
                ("get", json!({"id":1})),
                ("range", json!({"start":2,"end":3})),
                ("export", json!({"cursor":"protected","start":2,"end":3})),
                ("stats", json!({})),
            ]
        };
        let mut messages = ready();
        for (i, (name, arg)) in args.iter().enumerate() {
            messages.push(tool(i as i64 + 2, name, arg.clone()));
        }
        let mut calls = 0;
        let (outcome, output) = run(namespace, messages, |envelope| {
            assert_eq!(envelope["namespace"], namespace);
            let (name, arg) = &args[calls];
            let mut expected = arg.as_object().unwrap().clone();
            expected.insert("op".into(), json!(name));
            assert_eq!(envelope["request"], Value::Object(expected));
            calls += 1;
            Ok(json!({}))
        });
        assert_eq!(outcome, Ok(()));
        assert_eq!(calls, args.len());
        assert_eq!(output.len(), args.len() + 1);
    }
    let mut messages = ready();
    messages.push(request(2, "tools/list", json!({})));
    messages.push(tool(3, "history", json!({"key":"k"})));
    let (_, output) = run("capture-v1", messages, |_| panic!("invented history"));
    assert!(
        !output[1]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["name"] == "history")
    );
    assert_eq!(output[2]["error"]["code"], -32602);
}
#[test]
fn overrides_wrong_fields_cursor_rebinding_and_numeric_bounds_refuse_without_call() {
    let cases = [
        ("search", json!({"query":"q","op":"erase"})),
        ("search", json!({"query":"q","namespace":"capture-v1"})),
        ("search", json!({"query":"q","version":1})),
        ("search", json!({"query":"q","path":"private"})),
        ("search", json!({"query":"q","writer":"other"})),
        ("search", json!({"query":"q","owner":2})),
        ("search", json!({"query":"q","k":1001})),
        ("search", json!({"query":"q","from_us":12,"valid_at_us":11})),
        ("get", json!({"id":1,"key":"key"})),
        ("get", json!({"id":1.5})),
        ("range", json!({"cursor":"opaque","versions":"all"})),
        ("export", json!({"cursor":"opaque","key":"invented"})),
        ("stats", json!({"cursor":"opaque"})),
        ("history", json!({})),
        ("raw", json!({})),
        ("context", json!({})),
        ("erase", json!({})),
    ];
    let mut messages = ready();
    for (i, (name, args)) in cases.iter().enumerate() {
        messages.push(tool(i as i64 + 2, name, args.clone()));
    }
    let (outcome, output) = run("record-v2", messages, |_| {
        panic!("invalid arguments reached backend")
    });
    assert_eq!(outcome, Ok(()));
    for response in &output[1..] {
        assert_eq!(response["error"]["code"], -32602);
    }
    assert_eq!(output.len(), cases.len() + 1);
}
#[test]
fn backend_errors_and_oversized_whole_envelopes_are_content_free_tool_failures() {
    for value in [None, Some(json!({"data":"x".repeat(9*1024*1024)}))] {
        let mut messages = ready();
        messages.push(tool(2, "stats", json!({})));
        let (_, output) = run("record-v2", messages, |_| {
            value.clone().ok_or("private backend detail")
        });
        assert_eq!(output[1]["result"]["isError"], true);
        assert_eq!(
            output[1]["result"]["content"][0]["text"],
            "native_read_refused"
        );
        assert!(
            !serde_json::to_string(&output)
                .unwrap()
                .contains("private backend detail")
        );
        assert!(output[1]["result"].get("structuredContent").is_none());
    }
}
#[test]
fn malformed_batch_frames_ids_and_partial_eof_never_echo_input() {
    for (line, code) in [
        (b"{bad private input}\n".as_slice(), -32700),
        (b"[]\n".as_slice(), -32600),
        (
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"id\":2,\"method\":\"ping\"}\n".as_slice(),
            -32600,
        ),
        (
            b"{\"jsonrpc\":\"2.0\",\"id\":9007199254740992,\"method\":\"ping\"}\n".as_slice(),
            -32600,
        ),
        (
            b"{\"jsonrpc\":\"2.0\",\"id\":null,\"method\":\"ping\"}\n".as_slice(),
            -32600,
        ),
        (&b"\xff\n"[..], -32700),
    ] {
        let mut output = Vec::new();
        assert_eq!(
            mcp::serve(Cursor::new(line), &mut output, "record-v2", |_| panic!()),
            Ok(())
        );
        let response: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(response["error"]["code"], code);
        assert!(!String::from_utf8(output).unwrap().contains("private input"));
    }
    for line in [
        vec![b'x'; 64 * 1024 + 1],
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}".to_vec(),
    ] {
        let mut output = Vec::new();
        assert_eq!(
            mcp::serve(Cursor::new(line), &mut output, "record-v2", |_| panic!()),
            Err(())
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap()["error"]["code"],
            -32700
        );
    }
}
#[test]
fn unknown_methods_and_cursors_are_protocol_errors_and_invalid_namespace_is_inert() {
    let mut messages = ready();
    messages.push(request(2, "erase", json!({})));
    messages.push(request(3, "tools/list", json!({"cursor":"invented"})));
    let (_, output) = run("record-v2", messages, |_| panic!());
    assert_eq!(output[1]["error"]["code"], -32601);
    assert_eq!(output[2]["error"]["code"], -32602);
    let (outcome, output) = run("legacy", ready(), |_| panic!());
    assert_eq!(outcome, Err(()));
    assert!(output.is_empty());
}
#[test]
fn output_write_failure_propagates_without_followup_execution() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut bytes = serde_json::to_vec(&initialize()).unwrap();
    bytes.push(b'\n');
    assert_eq!(
        mcp::serve(Cursor::new(bytes), Broken, "record-v2", |_| panic!()),
        Err(())
    );
}

#[test]
fn full_native_integer_tokens_remain_exact_without_float_conversion() {
    let mut messages = ready();
    messages.push(tool(
        2,
        "get",
        json!({"id":u64::MAX,"from_us":i64::MIN,"valid_at_us":i64::MAX}),
    ));
    let (_, output) = run("record-v2", messages, |value| {
        assert_eq!(value["request"]["id"].as_u64(), Some(u64::MAX));
        assert_eq!(value["request"]["from_us"].as_i64(), Some(i64::MIN));
        assert_eq!(value["request"]["valid_at_us"].as_i64(), Some(i64::MAX));
        Ok(json!({"id":u64::MAX,"time":i64::MIN}))
    });
    assert_eq!(
        output[1]["result"]["structuredContent"]["data"]["id"].as_u64(),
        Some(u64::MAX)
    );
    let mut messages = ready();
    messages.push(tool(2, "range", json!({"start":u64::MAX-1,"end":u64::MAX})));
    let (_, output) = run("capture-v1", messages, |_| Ok(json!({})));
    assert_eq!(output[1]["result"]["isError"], false);
}

#[test]
fn empty_query_is_timeline_mode_in_both_namespaces() {
    for namespace in ["record-v2", "capture-v1"] {
        let mut messages = ready();
        messages.push(tool(2, "search", json!({"query":""})));
        let (_, output) = run(namespace, messages, |request| {
            assert_eq!(request["request"]["query"], "");
            Ok(json!({"hits":[]}))
        });
        assert_eq!(output[1]["result"]["isError"], false);
    }
}
