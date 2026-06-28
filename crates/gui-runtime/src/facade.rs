//! The host API as a single composite tree — the one source of truth for both **dispatch** and
//! the per-language **facade**.
//!
//! Every operation is an individual leaf struct that carries its own [`Facade`] descriptor and
//! its `call`; namespaces (`kv`, `fs`, `http`, `crypto`, …) are structs that *embed* their
//! children as fields, recursively. From this one tree we:
//!   * generate the ergonomic `host` object/table for JS and Lua ([`js_prelude`] / [`lua_prelude`])
//!     — so the three languages can never drift, and adding an op is one leaf, not three edits;
//!   * route [`Host::dispatch`](crate::host::Host::dispatch) by finding the leaf whose op-id
//!     matches (op-ids like `"kv.get"` stay stable, so the WASM ABI is untouched).
//!
//! ponytail: the tree is rebuilt per dispatch (≈40 unit structs, trivial) — handlers make few
//! host calls. The prelude strings are built once via `OnceLock`.

use std::sync::OnceLock;

use serde_json::Value;

use crate::host::{arg, scope_arg, status_codes, str_arg, Host, HostError, HttpRequest};

// --- the tree node trait -------------------------------------------------

/// One node in the host API tree: a namespace (has [`children`](Node::children)) or a leaf
/// (has an [`op`](Node::op) id + [`call`](Node::call)). Both carry a [`Facade`].
pub trait Node {
    /// Facade segment name (e.g. `"crypto"`, `"get"`, `"kvPrivate"`).
    fn name(&self) -> &'static str;
    /// Children for a namespace; empty for a leaf.
    fn children(&self) -> Vec<&dyn Node> {
        Vec::new()
    }
    /// Constants a namespace injects into every descendant op's args (e.g. `scope`).
    fn inject(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }
    /// Dispatch op-id for a leaf (e.g. `"kv.get"`); `None` for a namespace.
    fn op(&self) -> Option<&'static str> {
        None
    }
    /// How this node renders into the per-language facade.
    fn facade(&self) -> Facade {
        Facade::Namespace
    }
    /// Execute a leaf op. Namespaces are not callable.
    fn call(&self, _host: &mut Host, _args: &Value) -> Result<Value, HostError> {
        Err(HostError::Other(format!("'{}' is a namespace, not callable", self.name())))
    }
}

/// How a leaf is exposed in the generated facade.
pub enum Facade {
    /// A namespace `{ ...children }`.
    Namespace,
    /// A call with positional params (+ injected/own consts, + optional trailing `opts` spread).
    Op {
        params: &'static [P],
        consts: &'static [(&'static str, &'static str)],
        opts: bool,
    },
    /// A call taking one object passed straight through: `(params) => __c(op, params || {})`.
    Object,
    /// A constant value computed once at facade-build time: `name: __c(op, {})`.
    Value,
    /// A verbatim expression (for irregular shapes like the `response` arity overload).
    Raw { js: &'static str, lua: &'static str },
}

/// `op!(P::str("a"), ...)` / `op_opts!(...)` — a [`Facade::Op`] whose params are const-promoted
/// to `'static` (arrays of `const fn` calls aren't auto-promoted, so wrap them in `const { }`).
macro_rules! op {
    ($($p:expr),* $(,)?) => {
        Facade::Op { params: const { &[$($p),*] }, consts: &[], opts: false }
    };
}
macro_rules! op_opts {
    ($($p:expr),* $(,)?) => {
        Facade::Op { params: const { &[$($p),*] }, consts: &[], opts: true }
    };
}

/// A facade parameter: the function arg `name`, the args-object `field` it fills, and how it is
/// coerced in each language.
pub struct P {
    pub name: &'static str,
    pub field: &'static str,
    pub coerce: Coerce,
}

#[derive(Clone, Copy)]
pub enum Coerce {
    /// `String(x)` / `tostring(x)`.
    Str,
    /// `String(x || "d")` / `tostring(x or "d")`.
    StrDefault(&'static str),
    /// `x || "d"` / `x or "d"` (no string wrap — encodings, curves).
    Default(&'static str),
    /// `x | 0` / `x`.
    Int,
    /// `x === undefined ? null : x` / `x`.
    ValueOrNull,
    /// `x` / `x`.
    Raw,
}

impl P {
    const fn str(n: &'static str) -> P {
        P { name: n, field: n, coerce: Coerce::Str }
    }
    const fn val(n: &'static str) -> P {
        P { name: n, field: n, coerce: Coerce::ValueOrNull }
    }
    const fn raw(n: &'static str, f: &'static str) -> P {
        P { name: n, field: f, coerce: Coerce::Raw }
    }
    const fn int(n: &'static str, f: &'static str) -> P {
        P { name: n, field: f, coerce: Coerce::Int }
    }
    const fn def(n: &'static str, d: &'static str) -> P {
        P { name: n, field: n, coerce: Coerce::Default(d) }
    }
    const fn strdef(n: &'static str, d: &'static str) -> P {
        P { name: n, field: n, coerce: Coerce::StrDefault(d) }
    }
    /// The recurring `enc` → `encoding` (default `hex`) output-encoding param.
    const fn enc() -> P {
        P { name: "enc", field: "encoding", coerce: Coerce::Default("hex") }
    }
}

// --- node-declaration macros ---------------------------------------------

/// Declare a leaf op: a unit struct + its [`Node`] impl (name, op-id, facade, call). The call is
/// written `|host, args| <expr>` but the params bind directly to the method signature (no closure,
/// so a body that borrows from `args` via `?` doesn't trip the borrow checker).
macro_rules! leaf {
    ($ty:ident, $name:literal, $op:literal, $fac:expr, |$h:ident, $a:ident| $body:expr) => {
        #[derive(Default)]
        pub struct $ty;
        impl Node for $ty {
            fn name(&self) -> &'static str {
                $name
            }
            fn op(&self) -> Option<&'static str> {
                Some($op)
            }
            fn facade(&self) -> Facade {
                $fac
            }
            fn call(&self, $h: &mut Host, $a: &Value) -> Result<Value, HostError> {
                $body
            }
        }
    };
}

/// Declare a namespace: a struct embedding its children as fields + its [`Node`] impl.
macro_rules! ns {
    ($ty:ident, $name:literal, $inject:expr, [$($f:ident: $cty:ident),* $(,)?]) => {
        #[derive(Default)]
        pub struct $ty { $(pub $f: $cty),* }
        impl Node for $ty {
            fn name(&self) -> &'static str { $name }
            fn inject(&self) -> &'static [(&'static str, &'static str)] { $inject }
            fn children(&self) -> Vec<&dyn Node> { vec![$(&self.$f),*] }
        }
    };
}

// --- leaves: kv / fs / http / events / response --------------------------

leaf!(KvGet, "get", "kv.get", op!(P::str("key")), |h, a| h
    .kv_get(scope_arg(a), str_arg(a, "key")?));
leaf!(KvSet, "set", "kv.set", op!(P::str("key"), P::val("value")), |h, a| {
    h.kv_set(scope_arg(a), str_arg(a, "key")?, arg(a, "value"))?;
    Ok(Value::Null)
});
leaf!(KvAll, "all", "kv.all", op!(), |h, a| h.kv_all(scope_arg(a)));

leaf!(HttpFetch, "fetch", "http.fetch", op!(P::raw("req", "request")), |h, a| {
    Ok(h.http_fetch(HttpRequest::from_json(a.get("request").unwrap_or(&Value::Null))?)?.to_json())
});

leaf!(FsRead, "read", "fs.read", op!(P::str("path")), |h, a| h
    .fs_read(str_arg(a, "path")?));
leaf!(FsWrite, "write", "fs.write", op!(P::str("path"), P::str("data")), |h, a| {
    h.fs_write(str_arg(a, "path")?, str_arg(a, "data")?)?;
    Ok(Value::Null)
});
leaf!(FsList, "list", "fs.list", op!(P::strdef("prefix", "")), |h, a| h
    .fs_list(a.get("prefix").and_then(|v| v.as_str()).unwrap_or("")));

leaf!(Log, "log", "log", op!(P::str("level"), P::str("message")), |h, a| {
    let level = a.get("level").and_then(|v| v.as_str()).unwrap_or("info");
    h.log(level, str_arg(a, "message").unwrap_or(""));
    Ok(Value::Null)
});

fn do_emit(h: &mut Host, a: &Value) -> Result<Value, HostError> {
    h.emit(scope_arg(a), str_arg(a, "name")?, arg(a, "data"));
    Ok(Value::Null)
}
leaf!(
    Emit,
    "emit",
    "emit",
    Facade::Op { params: const { &[P::str("name"), P::val("data")] }, consts: &[("scope", "public")], opts: false },
    |h, a| do_emit(h, a)
);
leaf!(
    EmitPrivate,
    "emitPrivate",
    "emit",
    Facade::Op { params: const { &[P::str("name"), P::val("data")] }, consts: &[("scope", "private")], opts: false },
    |h, a| do_emit(h, a)
);

const RESPONSE_JS: &str = "(code, body) => { if (body === undefined) { body = code; code = 200; } \
return __c(\"respond\", { status: code | 0, body: body === undefined ? null : body }); }";
const RESPONSE_LUA: &str = "function(code, body) if body == nil then body = code; code = 200 end \
return __c(\"respond\", { status = code, body = body }) end";
leaf!(Response, "response", "respond", Facade::Raw { js: RESPONSE_JS, lua: RESPONSE_LUA }, |h, a| {
    let status = a.get("status").and_then(|v| v.as_u64()).unwrap_or(200);
    h.set_response(status.clamp(100, 599) as u16, arg(a, "body"));
    Ok(Value::Null)
});
leaf!(ResponseCode, "responseCode", "status_codes", Facade::Value, |_host, _args| Ok(
    Value::Object(status_codes())
));

// --- leaves: crypto (forward to the crypto router) -----------------------

macro_rules! crypto_leaf {
    ($ty:ident, $name:literal, $op:literal, $fac:expr) => {
        leaf!($ty, $name, $op, $fac, |_host, a| crate::crypto::dispatch($op, a));
    };
}

crypto_leaf!(Hash, "hash", "crypto.hash", op_opts!(P::str("algo"), P::str("data")));
crypto_leaf!(Hmac, "hmac", "crypto.hmac", op_opts!(P::str("algo"), P::str("key"), P::str("data")));
crypto_leaf!(
    VerifyHmac,
    "verifyHmac",
    "crypto.verifyHmac",
    op_opts!(P::str("algo"), P::str("key"), P::str("data"), P::str("signature"))
);
crypto_leaf!(RandomBytes, "randomBytes", "crypto.randomBytes", op!(P::int("n", "bytes"), P::enc()));
crypto_leaf!(Uuid, "uuid", "crypto.uuid", op!());
crypto_leaf!(
    Encode,
    "encode",
    "crypto.encode",
    op!(P::str("data"), P::def("from", "utf8"), P::def("to", "hex"))
);
crypto_leaf!(Pbkdf2, "pbkdf2", "crypto.pbkdf2", op_opts!(P::str("password"), P::str("salt")));
crypto_leaf!(Hkdf, "hkdf", "crypto.hkdf", Facade::Object);
crypto_leaf!(
    ConstantTimeEqual,
    "constantTimeEqual",
    "crypto.constantTimeEqual",
    op!(P::str("a"), P::str("b"), P::enc())
);

crypto_leaf!(AeadSeal, "seal", "crypto.aead.seal", Facade::Object);
crypto_leaf!(AeadOpen, "open", "crypto.aead.open", Facade::Object);

crypto_leaf!(Ed25519Keypair, "keypair", "crypto.ed25519.keypair", op!(P::enc()));
crypto_leaf!(Ed25519Sign, "sign", "crypto.ed25519.sign", op_opts!(P::str("privateKey"), P::str("data")));
crypto_leaf!(
    Ed25519Verify,
    "verify",
    "crypto.ed25519.verify",
    op_opts!(P::str("publicKey"), P::str("data"), P::str("signature"))
);

crypto_leaf!(EcdsaKeypair, "keypair", "crypto.ecdsa.keypair", op!(P::def("curve", "p256"), P::enc()));
crypto_leaf!(EcdsaSign, "sign", "crypto.ecdsa.sign", op_opts!(P::str("privateKey"), P::str("data")));
crypto_leaf!(
    EcdsaVerify,
    "verify",
    "crypto.ecdsa.verify",
    op_opts!(P::str("publicKey"), P::str("data"), P::str("signature"))
);

crypto_leaf!(RsaSign, "sign", "crypto.rsa.sign", op_opts!(P::str("privateKey"), P::str("data")));
crypto_leaf!(
    RsaVerify,
    "verify",
    "crypto.rsa.verify",
    op_opts!(P::str("publicKey"), P::str("data"), P::str("signature"))
);

crypto_leaf!(X25519Keypair, "keypair", "crypto.x25519.keypair", op!(P::enc()));
crypto_leaf!(
    X25519Derive,
    "deriveShared",
    "crypto.x25519.deriveShared",
    op_opts!(P::str("privateKey"), P::str("publicKey"))
);

// --- namespaces (embedding their children) -------------------------------

ns!(Kv, "kv", &[("scope", "public")], [get: KvGet, set: KvSet, all: KvAll]);
ns!(KvPrivate, "kvPrivate", &[("scope", "private")], [get: KvGet, set: KvSet, all: KvAll]);
ns!(Http, "http", &[], [fetch: HttpFetch]);
ns!(Fs, "fs", &[], [read: FsRead, write: FsWrite, list: FsList]);
ns!(Aead, "aead", &[], [seal: AeadSeal, open: AeadOpen]);
ns!(Ed25519, "ed25519", &[], [keypair: Ed25519Keypair, sign: Ed25519Sign, verify: Ed25519Verify]);
ns!(Ecdsa, "ecdsa", &[], [keypair: EcdsaKeypair, sign: EcdsaSign, verify: EcdsaVerify]);
ns!(Rsa, "rsa", &[], [sign: RsaSign, verify: RsaVerify]);
ns!(X25519, "x25519", &[], [keypair: X25519Keypair, derive_shared: X25519Derive]);
ns!(
    Crypto,
    "crypto",
    &[],
    [
        hash: Hash,
        hmac: Hmac,
        verify_hmac: VerifyHmac,
        random_bytes: RandomBytes,
        uuid: Uuid,
        encode: Encode,
        pbkdf2: Pbkdf2,
        hkdf: Hkdf,
        constant_time_equal: ConstantTimeEqual,
        aead: Aead,
        ed25519: Ed25519,
        ecdsa: Ecdsa,
        rsa: Rsa,
        x25519: X25519,
    ]
);
ns!(
    Root,
    "",
    &[],
    [
        kv: Kv,
        kv_private: KvPrivate,
        http: Http,
        fs: Fs,
        crypto: Crypto,
        log: Log,
        emit: Emit,
        emit_private: EmitPrivate,
        response: Response,
        response_code: ResponseCode,
    ]
);

/// The host API tree. Cheap to build (unit structs); rebuilt per dispatch.
pub fn root() -> Root {
    Root::default()
}

/// Find the leaf whose op-id is `op` (depth-first). Several facade nodes may share one op-id
/// (e.g. `kv`/`kvPrivate` both reach `kv.get`); they have identical `call`, so the first wins.
pub fn find_op<'a>(node: &'a dyn Node, op: &str) -> Option<&'a dyn Node> {
    if node.op() == Some(op) {
        return Some(node);
    }
    node.children().into_iter().find_map(|c| find_op(c, op))
}

// --- facade generation ---------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Lang {
    Js,
    Lua,
}

/// The JS `host` object, generated from the tree once.
pub fn js_prelude() -> &'static str {
    static P: OnceLock<String> = OnceLock::new();
    P.get_or_init(|| {
        let body = render_children(&root(), Lang::Js, &[]);
        format!(
            "globalThis.host = (() => {{\n  const __c = (op, args) => \
JSON.parse(__host_call(op, JSON.stringify(args || {{}})));\n  return {{\n{body}\n  }};\n}})();\n"
        )
    })
}

/// The Lua `host` table, generated from the tree once. `__host_call`/`__encode`/`__decode` are
/// bound in Rust by the Lua engine; this defines `__c`/`__merge` and returns the table.
pub fn lua_prelude() -> &'static str {
    static P: OnceLock<String> = OnceLock::new();
    P.get_or_init(|| {
        let body = render_children(&root(), Lang::Lua, &[]);
        format!(
            "local __c = function(op, args) return __decode(__host_call(op, __encode(args or {{}}))) end\n\
local function __merge(base, extra) if extra then for k, v in pairs(extra) do base[k] = v end end return base end\n\
return {{\n{body}\n}}\n"
        )
    })
}

fn render_children(node: &dyn Node, lang: Lang, injected: &[(&str, &str)]) -> String {
    let mut inj: Vec<(&str, &str)> = injected.to_vec();
    inj.extend(node.inject().iter().copied());
    node.children()
        .into_iter()
        .map(|c| render(c, lang, &inj))
        .collect::<Vec<_>>()
        .join(",\n")
}

fn render(node: &dyn Node, lang: Lang, injected: &[(&str, &str)]) -> String {
    let name = node.name();
    match node.facade() {
        Facade::Namespace => {
            let body = render_children(node, lang, injected);
            match lang {
                Lang::Js => format!("{name}: {{\n{body}\n}}"),
                Lang::Lua => format!("{name} = {{\n{body}\n}}"),
            }
        }
        Facade::Op { params, consts, opts } => {
            render_op(name, node.op().unwrap(), params, consts, opts, injected, lang)
        }
        Facade::Object => {
            let op = node.op().unwrap();
            match lang {
                Lang::Js => format!("{name}: (params) => __c({op:?}, params || {{}})"),
                Lang::Lua => format!("{name} = function(params) return __c({op:?}, params or {{}}) end"),
            }
        }
        Facade::Value => {
            let op = node.op().unwrap();
            match lang {
                Lang::Js => format!("{name}: __c({op:?}, {{}})"),
                Lang::Lua => format!("{name} = __c({op:?}, {{}})"),
            }
        }
        Facade::Raw { js, lua } => match lang {
            Lang::Js => format!("{name}: {js}"),
            Lang::Lua => format!("{name} = {lua}"),
        },
    }
}

fn render_op(
    name: &str,
    op: &str,
    params: &[P],
    consts: &[(&str, &str)],
    opts: bool,
    injected: &[(&str, &str)],
    lang: Lang,
) -> String {
    let mut argnames: Vec<&str> = params.iter().map(|p| p.name).collect();
    if opts {
        argnames.push("opts");
    }
    let args = argnames.join(", ");

    let mut fields: Vec<String> = Vec::new();
    for (f, v) in injected.iter().chain(consts.iter()) {
        fields.push(match lang {
            Lang::Js => format!("{f}: {v:?}"),
            Lang::Lua => format!("{f} = {v:?}"),
        });
    }
    for p in params {
        let expr = coerce_expr(p, lang);
        fields.push(match lang {
            Lang::Js => format!("{}: {}", p.field, expr),
            Lang::Lua => format!("{} = {}", p.field, expr),
        });
    }
    let body = fields.join(", ");
    match lang {
        Lang::Js if opts => format!("{name}: ({args}) => __c({op:?}, {{ {body}, ...(opts || {{}}) }})"),
        Lang::Js => format!("{name}: ({args}) => __c({op:?}, {{ {body} }})"),
        Lang::Lua if opts => format!("{name} = function({args}) return __c({op:?}, __merge({{ {body} }}, opts)) end"),
        Lang::Lua => format!("{name} = function({args}) return __c({op:?}, {{ {body} }}) end"),
    }
}

fn coerce_expr(p: &P, lang: Lang) -> String {
    let n = p.name;
    match (lang, p.coerce) {
        (Lang::Js, Coerce::Str) => format!("String({n})"),
        (Lang::Lua, Coerce::Str) => format!("tostring({n})"),
        (Lang::Js, Coerce::StrDefault(d)) => format!("String({n} || {d:?})"),
        (Lang::Lua, Coerce::StrDefault(d)) => format!("tostring({n} or {d:?})"),
        (Lang::Js, Coerce::Default(d)) => format!("{n} || {d:?}"),
        (Lang::Lua, Coerce::Default(d)) => format!("{n} or {d:?}"),
        (Lang::Js, Coerce::Int) => format!("{n} | 0"),
        (Lang::Lua, Coerce::Int) => n.to_string(),
        (Lang::Js, Coerce::ValueOrNull) => format!("{n} === undefined ? null : {n}"),
        (Lang::Lua, Coerce::ValueOrNull) => n.to_string(),
        (_, Coerce::Raw) => n.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_op_id_resolves() {
        // Each leaf the renderer emits must be reachable by dispatch (op-id present in the tree).
        for op in [
            "kv.get", "kv.set", "kv.all", "http.fetch", "fs.read", "fs.write", "fs.list", "log",
            "emit", "respond", "status_codes", "crypto.hash", "crypto.aead.seal",
            "crypto.ed25519.sign", "crypto.x25519.deriveShared",
        ] {
            assert!(find_op(&root(), op).is_some(), "op {op:?} not found in tree");
        }
        assert!(find_op(&root(), "nope").is_none());
    }

    #[test]
    fn generated_facades_have_expected_shapes() {
        let js = js_prelude();
        // namespace + injected scope const + String coercion
        assert!(js.contains(r#"get: (key) => __c("kv.get", { scope: "public", key: String(key) })"#));
        assert!(js.contains(r#"get: (key) => __c("kv.get", { scope: "private", key: String(key) })"#));
        // opts spread, field rename, value-or-null
        assert!(js.contains(r#"hash: (algo, data, opts) => __c("crypto.hash", { algo: String(algo), data: String(data), ...(opts || {}) })"#));
        assert!(js.contains(r#"randomBytes: (n, enc) => __c("crypto.randomBytes", { bytes: n | 0, encoding: enc || "hex" })"#));
        assert!(js.contains(r#"emit: (name, data) => __c("emit", { scope: "public", name: String(name), data: data === undefined ? null : data })"#));
        // object passthrough + value
        assert!(js.contains(r#"hkdf: (params) => __c("crypto.hkdf", params || {})"#));
        assert!(js.contains(r#"responseCode: __c("status_codes", {})"#));

        let lua = lua_prelude();
        assert!(lua.contains(r#"get = function(key) return __c("kv.get", { scope = "public", key = tostring(key) }) end"#));
        assert!(lua.contains(r#"hash = function(algo, data, opts) return __c("crypto.hash", __merge({ algo = tostring(algo), data = tostring(data) }, opts)) end"#));
    }
}
