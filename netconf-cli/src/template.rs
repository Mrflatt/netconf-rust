use crate::inventory::Vars;
use netconf_async::error::{NetconfClientError, NetconfClientResult};
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Render a Go-template subset: `{{ .field }}`, `{{ env "NAME" }}`, `{{ range . }}`.
pub fn render(source: &str, vars: &Vars) -> NetconfClientResult<String> {
    let tokens = lex(source)?;
    let nodes = parse_nodes(&tokens)?;
    render_nodes(&nodes, Context::from_vars(vars))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ActionKind {
    Field(String),
    Env(String),
    Range,
    End,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Text(String),
    Action(ActionKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Text(String),
    Field(String),
    Env(String),
    Range(Vec<Node>),
}

enum Context<'a> {
    Map(&'a BTreeMap<String, String>),
    List(&'a [BTreeMap<String, String>]),
}

impl<'a> Context<'a> {
    fn from_vars(vars: &'a Vars) -> Self {
        match vars {
            Vars::None => Context::Map(empty_map()),
            Vars::Map(map) => Context::Map(map),
            Vars::List(rows) => Context::List(rows),
        }
    }
}

fn empty_map() -> &'static BTreeMap<String, String> {
    static EMPTY: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    EMPTY.get_or_init(BTreeMap::new)
}

fn lex(source: &str) -> NetconfClientResult<Vec<Token>> {
    let mut staged: Vec<(Token, bool)> = Vec::new();
    let mut i = 0;
    while i < source.len() {
        match source[i..].find("{{") {
            None => {
                staged.push((Token::Text(source[i..].to_string()), false));
                break;
            }
            Some(rel) => {
                if rel > 0 {
                    staged.push((Token::Text(source[i..i + rel].to_string()), false));
                }
                i += rel + 2;
                let Some(end) = source[i..].find("}}") else {
                    return Err(template_error("unclosed template action"));
                };
                let raw = &source[i..i + end];
                i += end + 2;
                let (kind, left_trim, right_trim) = parse_action(raw)?;
                if left_trim && let Some((Token::Text(text), _)) = staged.last_mut() {
                    let trimmed = text.trim_end();
                    text.truncate(trimmed.len());
                }
                staged.push((Token::Action(kind), right_trim));
            }
        }
    }
    Ok(apply_right_trims(staged))
}

fn apply_right_trims(tokens: Vec<(Token, bool)>) -> Vec<Token> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut trim_next = false;
    for (token, right_trim) in tokens {
        let token = if trim_next {
            match token {
                Token::Text(text) => Token::Text(text.trim_start().to_string()),
                other => other,
            }
        } else {
            token
        };
        trim_next = right_trim;
        out.push(token);
    }
    out
}

fn parse_action(raw: &str) -> NetconfClientResult<(ActionKind, bool, bool)> {
    let mut body = raw;
    let left_trim = body.starts_with('-');
    if left_trim {
        body = body[1..].trim_start();
    } else {
        body = body.trim_start();
    }
    let right_trim = body.ends_with('-');
    if right_trim {
        body = body[..body.len() - 1].trim_end();
    } else {
        body = body.trim_end();
    }
    Ok((parse_action_kind(body)?, left_trim, right_trim))
}

fn parse_action_kind(body: &str) -> NetconfClientResult<ActionKind> {
    if body == "end" {
        return Ok(ActionKind::End);
    }
    if let Some(rest) = body.strip_prefix("range") {
        let rest = rest.trim();
        if rest == "." {
            return Ok(ActionKind::Range);
        }
        return Err(template_error(&format!(
            "unsupported range target '{rest}' (use range .)"
        )));
    }
    if let Some(rest) = body.strip_prefix("env") {
        return Ok(ActionKind::Env(parse_quoted(rest.trim())?));
    }
    if let Some(field) = body.strip_prefix('.') {
        if is_ident(field) {
            return Ok(ActionKind::Field(field.to_string()));
        }
        return Err(template_error(&format!(
            "invalid template field '.{field}'"
        )));
    }
    Err(template_error(&format!("unknown template action '{body}'")))
}

fn parse_quoted(value: &str) -> NetconfClientResult<String> {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let quote = bytes[0];
        if (quote == b'"' || quote == b'\'') && bytes[bytes.len() - 1] == quote {
            return Ok(value[1..value.len() - 1].to_string());
        }
    }
    Err(template_error(&format!(
        "env expects a quoted name, got '{value}'"
    )))
}

fn is_ident(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn parse_nodes(tokens: &[Token]) -> NetconfClientResult<Vec<Node>> {
    let mut i = 0;
    parse_until(tokens, &mut i, false)
}

fn parse_until(tokens: &[Token], i: &mut usize, in_range: bool) -> NetconfClientResult<Vec<Node>> {
    let mut nodes = Vec::new();
    while *i < tokens.len() {
        match &tokens[*i] {
            Token::Text(text) => {
                nodes.push(Node::Text(text.clone()));
                *i += 1;
            }
            Token::Action(ActionKind::Field(name)) => {
                nodes.push(Node::Field(name.clone()));
                *i += 1;
            }
            Token::Action(ActionKind::Env(name)) => {
                nodes.push(Node::Env(name.clone()));
                *i += 1;
            }
            Token::Action(ActionKind::Range) => {
                *i += 1;
                let inner = parse_until(tokens, i, true)?;
                nodes.push(Node::Range(inner));
            }
            Token::Action(ActionKind::End) => {
                if !in_range {
                    return Err(template_error("unmatched end"));
                }
                *i += 1;
                return Ok(nodes);
            }
        }
    }
    if in_range {
        return Err(template_error("unclosed range"));
    }
    Ok(nodes)
}

fn render_nodes(nodes: &[Node], ctx: Context<'_>) -> NetconfClientResult<String> {
    let mut out = String::new();
    for node in nodes {
        match node {
            Node::Text(text) => out.push_str(text),
            Node::Field(name) => out.push_str(lookup_field(name, &ctx)?),
            Node::Env(name) => {
                if let Ok(value) = std::env::var(name) {
                    out.push_str(&value);
                }
            }
            Node::Range(inner) => {
                let rows: Vec<&BTreeMap<String, String>> = match ctx {
                    Context::List(rows) => rows.iter().collect(),
                    Context::Map(map) => vec![map],
                };
                for row in rows {
                    out.push_str(&render_nodes(inner, Context::Map(row))?);
                }
            }
        }
    }
    Ok(out)
}

fn lookup_field<'a>(name: &str, ctx: &'a Context<'a>) -> NetconfClientResult<&'a str> {
    match ctx {
        Context::Map(map) => map
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| template_error(&format!("missing template field '{name}'"))),
        Context::List(_) => Err(template_error(&format!(
            "field '{name}' requires a single-row inventory or {{{{ range . }}}}"
        ))),
    }
}

fn template_error(message: &str) -> NetconfClientError {
    NetconfClientError::new(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn render_field_and_env() {
        unsafe { std::env::set_var("SERVICE_NAME", "VPLS-INTERNET") };
        let src =
            r#"<service-name>{{ env "SERVICE_NAME" }}</service-name><sap-id>{{ .sapId }}</sap-id>"#;
        let vars = Vars::Map(map(&[("sapId", "1/1/1:40")]));
        assert_eq!(
            render(src, &vars).unwrap(),
            "<service-name>VPLS-INTERNET</service-name><sap-id>1/1/1:40</sap-id>"
        );
    }

    #[test]
    fn render_range_matches_go_port_template() {
        let src = r#"<config xmlns:nc="urn:ietf:params:xml:ns:netconf:base:1.0">
    <configure xmlns="urn:nokia.com:sros:ns:yang:sr:conf">
        {{- range . }}
        <port>
            <port-id>{{ .portId }}</port-id>
            <description nc:operation="merge">{{ .description }}</description>
        </port>
        {{- end }}
    </configure>
</config>
"#;
        let vars = Vars::List(vec![
            map(&[("portId", "1/1/1"), ("description", "agg-link 1/1/1")]),
            map(&[("portId", "1/1/2"), ("description", "agg-link 1/1/2")]),
        ]);
        let expected = r#"<config xmlns:nc="urn:ietf:params:xml:ns:netconf:base:1.0">
    <configure xmlns="urn:nokia.com:sros:ns:yang:sr:conf">
        <port>
            <port-id>1/1/1</port-id>
            <description nc:operation="merge">agg-link 1/1/1</description>
        </port>
        <port>
            <port-id>1/1/2</port-id>
            <description nc:operation="merge">agg-link 1/1/2</description>
        </port>
    </configure>
</config>
"#;
        assert_eq!(render(src, &vars).unwrap(), expected);
    }

    #[test]
    fn missing_field_and_unknown_action_are_errors() {
        let vars = Vars::Map(map(&[("a", "1")]));
        assert!(
            render("{{ .b }}", &vars)
                .unwrap_err()
                .to_string()
                .contains("missing")
        );
        assert!(
            render("{{ uuid }}", &vars)
                .unwrap_err()
                .to_string()
                .contains("unknown")
        );
    }

    #[test]
    fn list_without_range_is_error() {
        let vars = Vars::List(vec![map(&[("portId", "1")])]);
        let err = render("{{ .portId }}", &vars).unwrap_err();
        assert!(err.to_string().contains("range"), "{err}");
    }

    #[test]
    fn identity_without_actions() {
        assert_eq!(render("<ok/>", &Vars::None).unwrap(), "<ok/>");
    }

    #[test]
    fn render_sap_modify_matches_go() {
        unsafe { std::env::set_var("SERVICE_NAME", "VPLS-INTERNET") };
        let src = r#"<config xmlns:nc="urn:ietf:params:xml:ns:netconf:base:1.0">
    <configure xmlns="urn:nokia.com:sros:ns:yang:sr:conf">
        <service>
            <vpls>
                <service-name>{{ env "SERVICE_NAME" }}</service-name>
                <sap nc:operation="merge">
                    <sap-id>{{ .sapId }}</sap-id>
                    <split-horizon-group>SHG</split-horizon-group>
                    <ingress>
                        <qos>
                            <sap-ingress>
                                <policy-name>{{ .ingressQos }}</policy-name>
                            </sap-ingress>
                        </qos>
                    </ingress>
                    <egress>
                        <qos>
                            <vlan-qos-policy>
                                <policy-name>{{ .vlanQos }}</policy-name>
                            </vlan-qos-policy>
                            <egress-remark-policy>
                                <policy-name>{{ .egressRemarkPolicy }}</policy-name>
                            </egress-remark-policy>
                        </qos>
                    </egress>
                </sap>
            </vpls>
        </service>
    </configure>
</config>
"#;
        let vars = Vars::Map(map(&[
            ("sapId", "1/1/1:40"),
            ("ingressQos", "10100"),
            ("vlanQos", "10100"),
            ("egressRemarkPolicy", "10000"),
        ]));
        let expected = r#"<config xmlns:nc="urn:ietf:params:xml:ns:netconf:base:1.0">
    <configure xmlns="urn:nokia.com:sros:ns:yang:sr:conf">
        <service>
            <vpls>
                <service-name>VPLS-INTERNET</service-name>
                <sap nc:operation="merge">
                    <sap-id>1/1/1:40</sap-id>
                    <split-horizon-group>SHG</split-horizon-group>
                    <ingress>
                        <qos>
                            <sap-ingress>
                                <policy-name>10100</policy-name>
                            </sap-ingress>
                        </qos>
                    </ingress>
                    <egress>
                        <qos>
                            <vlan-qos-policy>
                                <policy-name>10100</policy-name>
                            </vlan-qos-policy>
                            <egress-remark-policy>
                                <policy-name>10000</policy-name>
                            </egress-remark-policy>
                        </qos>
                    </egress>
                </sap>
            </vpls>
        </service>
    </configure>
</config>
"#;
        assert_eq!(render(src, &vars).unwrap(), expected);
    }
}
