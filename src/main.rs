//! espo: a token-efficient EspoCRM CLI for humans and AI agents.

mod client;
mod config;
mod meta;
mod output;
mod query;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use client::{ApiError, Client, EXIT_ERROR, usage};
use config::{Config, Profile};
use meta::Meta;
use serde_json::{Map, Value, json};
use std::process::ExitCode;
use std::time::SystemTime;

#[derive(Parser)]
#[command(
    name = "espo",
    version,
    about = "EspoCRM from the command line, built for AI agents",
    long_about = "Compact TSV output on stdout, counters on stderr. Authenticate once with `espo auth login`."
)]
struct Cli {
    /// Configuration profile to use
    #[arg(long, global = true)]
    profile: Option<String>,
    /// Print the raw JSON response instead of TSV
    #[arg(long, global = true)]
    json: bool,
    /// Print the request that would be sent and exit
    #[arg(long, global = true)]
    dry_run: bool,
    /// Refetch the metadata cache before running
    #[arg(long, global = true)]
    refresh: bool,
    /// Omit the TSV header row
    #[arg(long, global = true)]
    no_header: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage stored credentials
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Show the active profile, the instance it points at and the cache state
    Status,
    /// List entity types available on this instance
    Entities {
        /// Only entities users see as records
        #[arg(short, long)]
        objects: bool,
    },
    /// Show fields or links of an entity type
    Schema {
        entity: String,
        /// Show only this field
        #[arg(short, long)]
        field: Option<String>,
        /// Show links instead of fields
        #[arg(short, long)]
        links: bool,
    },
    /// Search records
    List(ListArgs),
    /// Read one record
    Get {
        entity: String,
        id: String,
        /// Comma-separated attributes, or '*' for all
        #[arg(short, long)]
        select: Option<String>,
    },
    /// Create a record from field=value pairs
    Create {
        entity: String,
        /// field=value, or field:=<json> for a raw value
        #[arg(num_args = 0..)]
        fields: Vec<String>,
        /// JSON object body, or '-' to read it from stdin
        #[arg(long)]
        data: Option<String>,
    },
    /// Update a record from field=value pairs
    Update {
        entity: String,
        id: String,
        /// field=value, or field:=<json> for a raw value
        #[arg(num_args = 0..)]
        fields: Vec<String>,
        /// JSON object body, or '-' to read it from stdin
        #[arg(long)]
        data: Option<String>,
    },
    /// Delete a record
    Delete {
        entity: String,
        id: String,
        /// Required: deletion is not reversible
        #[arg(long)]
        yes: bool,
    },
    /// Relate records through a link
    Link {
        entity: String,
        id: String,
        link: String,
        #[arg(required = true, num_args = 1..)]
        targets: Vec<String>,
    },
    /// Unrelate records through a link
    Unlink {
        entity: String,
        id: String,
        link: String,
        #[arg(required = true, num_args = 1..)]
        targets: Vec<String>,
    },
    /// Search records on the far side of a link
    Related(RelatedArgs),
    /// Run a List report and print its rows
    Report {
        id: String,
        /// Maximum rows
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: u32,
        #[arg(long, default_value_t = 0)]
        offset: u32,
    },
    /// Call any API endpoint directly
    Raw {
        method: String,
        /// Path after /api/v1, for example Note or Lead/abc123/convert
        path: String,
        /// JSON object body, or '-' to read it from stdin
        #[arg(long)]
        data: Option<String>,
        /// Extra query parameter as key=value
        #[arg(short, long = "query")]
        queries: Vec<String>,
        /// Extra request header as Name:value
        #[arg(short = 'H', long = "header")]
        headers: Vec<String>,
    },
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Store and verify credentials for a profile
    Login {
        #[arg(long)]
        url: String,
        /// API key; read from stdin when omitted
        #[arg(long)]
        api_key: Option<String>,
        /// Make this the default profile
        #[arg(long)]
        set_default: bool,
    },
    /// Verify the active profile against the instance
    Status,
    /// Remove a stored profile
    Logout,
}

#[derive(Args)]
struct ListArgs {
    entity: String,
    /// Filter, repeatable and ANDed: status=New, name~puig, amount>=5, status:New,Dead, field=null
    #[arg(short = 'w', long = "where")]
    filters: Vec<String>,
    /// Raw Espo where clause as a JSON array, for OR and nesting
    #[arg(long)]
    where_json: Option<String>,
    /// Maximum records
    #[arg(short = 'n', long, default_value_t = 20)]
    limit: u32,
    #[arg(long, default_value_t = 0)]
    offset: u32,
    /// Attribute to sort by
    #[arg(long)]
    order: Option<String>,
    /// Sort descending
    #[arg(long)]
    desc: bool,
    /// Comma-separated attributes, or '*' for all
    #[arg(short, long)]
    select: Option<String>,
    /// Skip the total count, faster on large entities
    #[arg(long)]
    no_total: bool,
}

#[derive(Args)]
struct RelatedArgs {
    entity: String,
    id: String,
    link: String,
    #[arg(short = 'w', long = "where")]
    filters: Vec<String>,
    #[arg(long)]
    where_json: Option<String>,
    #[arg(short = 'n', long, default_value_t = 20)]
    limit: u32,
    #[arg(long, default_value_t = 0)]
    offset: u32,
    #[arg(long)]
    order: Option<String>,
    #[arg(long)]
    desc: bool,
    #[arg(short, long)]
    select: Option<String>,
    #[arg(long)]
    no_total: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            let code = e.downcast_ref::<ApiError>().map(|a| a.code).unwrap_or(EXIT_ERROR);
            ExitCode::from(code)
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    if let Command::Auth { command } = &cli.command {
        return auth(&cli, command);
    }
    let cfg = Config::load()?;
    let (profile_name, profile) = cfg.resolve(cli.profile.as_deref())?;
    let client = Client::new(&profile.url, &profile.api_key, cli.dry_run)?;
    let header = !cli.no_header;

    match &cli.command {
        Command::Auth { .. } => unreachable!("handled above"),

        Command::Status => status(&client, &profile_name, &profile, cli.json, header)?,

        Command::Entities { objects } => {
            let meta = Meta::load(&client, &profile_name, &profile.url, cli.refresh)?;
            let rows = meta.entities(*objects);
            if cli.json {
                let v: Vec<Value> = rows
                    .iter()
                    .map(|(n, m, o)| json!({"name": n, "module": m, "object": o}))
                    .collect();
                output::print_json(&Value::Array(v));
            } else {
                if header {
                    println!("entity\tmodule\tobject");
                }
                for (name, module, object) in &rows {
                    println!("{name}\t{module}\t{object}");
                }
            }
            eprintln!("{} entities", rows.len());
        }

        Command::Schema { entity, field, links } => {
            let meta = Meta::load(&client, &profile_name, &profile.url, cli.refresh)?;
            let entity = meta.resolve_entity(entity)?;
            schema(&meta, &entity, field.as_deref(), *links, cli.json, header)?;
        }

        Command::List(a) => {
            let meta = Meta::load(&client, &profile_name, &profile.url, cli.refresh)?;
            let entity = meta.resolve_entity(&a.entity)?;
            let cols = select_columns(&meta, &entity, a.select.as_deref(), &a.filters, a.order.as_deref());
            let params = search_params(
                &meta,
                &entity,
                &a.filters,
                a.where_json.as_deref(),
                a.limit,
                a.offset,
                a.order.as_deref(),
                a.desc,
                &cols,
            )?;
            let resp = list_request(&client, &entity, &params, a.no_total)?;
            emit_list(&resp, &cols, cli.json, header, client.dry_run);
        }

        Command::Related(a) => {
            let meta = Meta::load(&client, &profile_name, &profile.url, cli.refresh)?;
            let entity = meta.resolve_entity(&a.entity)?;
            let target = meta
                .link_entity(&entity, &a.link)
                .ok_or_else(|| usage(format!("{entity} has no link {:?}; run: espo schema {entity} --links", a.link)))?
                .to_string();
            let cols = select_columns(&meta, &target, a.select.as_deref(), &a.filters, a.order.as_deref());
            let params = search_params(
                &meta,
                &target,
                &a.filters,
                a.where_json.as_deref(),
                a.limit,
                a.offset,
                a.order.as_deref(),
                a.desc,
                &cols,
            )?;
            let path = format!("{entity}/{}/{}", a.id, a.link);
            let resp = list_request(&client, &path, &params, a.no_total)?;
            emit_list(&resp, &cols, cli.json, header, client.dry_run);
        }

        Command::Get { entity, id, select } => {
            let meta = Meta::load(&client, &profile_name, &profile.url, cli.refresh)?;
            let entity = meta.resolve_entity(entity)?;
            let cols = explicit_columns(select.as_deref());
            let mut query: Vec<(&str, String)> = Vec::new();
            if !cols.is_empty() {
                query.push(("select", cols.join(",")));
            }
            let resp = client.request("GET", &format!("{entity}/{id}"), &query, None, &[])?;
            if client.dry_run {
                return Ok(());
            }
            if cli.json {
                output::print_json(&resp);
            } else {
                let obj = resp.as_object().ok_or_else(|| usage("unexpected response shape"))?;
                output::print_record(obj, &cols);
            }
        }

        Command::Create { entity, fields, data } => {
            let meta = Meta::load(&client, &profile_name, &profile.url, cli.refresh)?;
            let entity = meta.resolve_entity(entity)?;
            let body = build_body(&meta, &entity, fields, data.as_deref())?;
            if body.is_empty() {
                return Err(usage("nothing to create; pass field=value pairs or --data"));
            }
            let resp = client.request("POST", &entity, &[], Some(&Value::Object(body)), &[])?;
            emit_written(&resp, cli.json, client.dry_run);
        }

        Command::Update { entity, id, fields, data } => {
            let meta = Meta::load(&client, &profile_name, &profile.url, cli.refresh)?;
            let entity = meta.resolve_entity(entity)?;
            let body = build_body(&meta, &entity, fields, data.as_deref())?;
            if body.is_empty() {
                return Err(usage("nothing to update; pass field=value pairs or --data"));
            }
            let resp =
                client.request("PUT", &format!("{entity}/{id}"), &[], Some(&Value::Object(body)), &[])?;
            emit_written(&resp, cli.json, client.dry_run);
        }

        Command::Delete { entity, id, yes } => {
            if !yes {
                return Err(usage("delete is not reversible; pass --yes to confirm"));
            }
            let meta = Meta::load(&client, &profile_name, &profile.url, cli.refresh)?;
            let entity = meta.resolve_entity(entity)?;
            client.request("DELETE", &format!("{entity}/{id}"), &[], None, &[])?;
            if !client.dry_run {
                println!("{id}");
                eprintln!("deleted");
            }
        }

        Command::Link { entity, id, link, targets } | Command::Unlink { entity, id, link, targets } => {
            let meta = Meta::load(&client, &profile_name, &profile.url, cli.refresh)?;
            let entity = meta.resolve_entity(entity)?;
            if meta.link_entity(&entity, link).is_none() {
                return Err(usage(format!(
                    "{entity} has no link {link:?}; run: espo schema {entity} --links"
                )));
            }
            let method = if matches!(cli.command, Command::Link { .. }) { "POST" } else { "DELETE" };
            let body = json!({"ids": targets});
            client.request(method, &format!("{entity}/{id}/{link}"), &[], Some(&body), &[])?;
            if !client.dry_run {
                eprintln!("{} {} target(s)", if method == "POST" { "linked" } else { "unlinked" }, targets.len());
            }
        }

        Command::Report { id, limit, offset } => {
            let query = [("id", id.clone()), ("maxSize", limit.to_string()), ("offset", offset.to_string())];
            let resp = client.request("GET", "Report/action/runList", &query, None, &[])?;
            let mut cols: Vec<String> = resp
                .get("columns")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
                .unwrap_or_default();
            // The report never lists id, and without it a row cannot be looked up.
            if !cols.is_empty() && !cols.iter().any(|c| c == "id") {
                cols.insert(0, "id".to_string());
            }
            emit_list(&resp, &cols, cli.json, header, client.dry_run);
        }

        Command::Raw { method, path, data, queries, headers } => {
            let body = match data.as_deref() {
                Some(s) => Some(read_json(s)?),
                None => None,
            };
            let mut query: Vec<(&str, String)> = Vec::new();
            let pairs: Vec<(String, String)> = queries
                .iter()
                .map(|q| match q.split_once('=') {
                    Some((k, v)) => Ok((k.to_string(), v.to_string())),
                    None => Err(usage(format!("expected key=value, got {q:?}"))),
                })
                .collect::<Result<_>>()?;
            for (k, v) in &pairs {
                query.push((k.as_str(), v.clone()));
            }
            let extra: Vec<(&str, &str)> = headers
                .iter()
                .map(|h| match h.split_once(':') {
                    Some((n, v)) => Ok((n.trim(), v.trim())),
                    None => Err(usage(format!("expected Name:value, got {h:?}"))),
                })
                .collect::<Result<_>>()?;
            let resp = client.request(&method.to_uppercase(), path, &query, body.as_ref(), &extra)?;
            if !client.dry_run {
                output::print_json(&resp);
            }
        }
    }
    Ok(())
}

fn auth(cli: &Cli, command: &AuthCommand) -> Result<()> {
    let mut cfg = Config::load()?;
    match command {
        AuthCommand::Login { url, api_key, set_default } => {
            let key = match api_key {
                Some(k) => k.clone(),
                None => {
                    let mut buf = String::new();
                    std::io::stdin().read_line(&mut buf).context("reading api key from stdin")?;
                    buf.trim().to_string()
                }
            };
            if key.is_empty() {
                return Err(usage("empty api key"));
            }
            let name = cli.profile.clone().unwrap_or_else(|| "default".into());
            config::check_profile_name(&name)?;
            let client = Client::new(url, &key, false)?;
            let who = client.request("GET", "App/user", &[], None, &[])?;
            let user = who.get("user").unwrap_or(&Value::Null);
            cfg.profiles.insert(name.clone(), Profile { url: url.clone(), api_key: key });
            if *set_default || cfg.default.is_none() {
                cfg.default = Some(name.clone());
            }
            cfg.save()?;
            println!(
                "{name}\t{}\t{}",
                user.get("userName").and_then(Value::as_str).unwrap_or("?"),
                user.get("type").and_then(Value::as_str).unwrap_or("?")
            );
            eprintln!("saved profile {name} to {}", config::config_path().display());
        }
        AuthCommand::Status => {
            let (name, profile) = cfg.resolve(cli.profile.as_deref())?;
            let client = Client::new(&profile.url, &profile.api_key, false)?;
            status(&client, &name, &profile, cli.json, !cli.no_header)?;
        }
        AuthCommand::Logout => {
            let name = cli
                .profile
                .clone()
                .or_else(|| cfg.default.clone())
                .ok_or_else(|| usage("no profile to remove"))?;
            config::check_profile_name(&name)?;
            if cfg.profiles.remove(&name).is_none() {
                return Err(usage(format!("unknown profile {name:?}")));
            }
            if cfg.default.as_deref() == Some(name.as_str()) {
                cfg.default = cfg.profiles.keys().next().cloned();
            }
            cfg.save()?;
            eprintln!("removed profile {name}");
        }
    }
    Ok(())
}

/// One App/user call plus the local cache; never downloads metadata just to report a count.
fn status(client: &Client, name: &str, profile: &Profile, as_json: bool, header: bool) -> Result<()> {
    let who = client.send("GET", "App/user", &[], None, &[])?;
    let user = who.get("user").unwrap_or(&Value::Null);
    let settings = who.get("settings").unwrap_or(&Value::Null);
    let field = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("?").to_string();

    let cached = Meta::cached(name, &profile.url);
    let entities = cached.as_ref().map(|(m, _)| m.entities(false).len());
    let cache = match &cached {
        Some((_, at)) => match SystemTime::now().duration_since(*at) {
            Ok(age) => format!("{} ago", humanize(age)),
            Err(_) => "just now".to_string(),
        },
        None => "empty".to_string(),
    };

    let rows: Vec<(&str, String)> = vec![
        ("profile", name.to_string()),
        ("url", profile.url.clone()),
        ("instance", field(settings, "applicationName")),
        ("version", field(settings, "version")),
        ("user", field(user, "userName")),
        ("type", field(user, "type")),
        ("entities", entities.map_or_else(|| "-".to_string(), |n| n.to_string())),
        ("cache", cache),
    ];

    if as_json {
        let obj: Map<String, Value> =
            rows.into_iter().map(|(k, v)| (k.to_string(), Value::String(v))).collect();
        output::print_json(&Value::Object(obj));
    } else {
        if header {
            println!("key\tvalue");
        }
        for (k, v) in rows {
            println!("{k}\t{v}");
        }
    }
    Ok(())
}

fn humanize(d: std::time::Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

fn schema(meta: &Meta, entity: &str, field: Option<&str>, links: bool, as_json: bool, header: bool) -> Result<()> {
    if links {
        let rows = meta.link_list(entity);
        if as_json {
            let v: Vec<Value> = rows
                .iter()
                .map(|l| json!({"link": l.name, "type": l.kind, "entity": l.entity}))
                .collect();
            output::print_json(&Value::Array(v));
        } else {
            if header {
                println!("link\ttype\tentity");
            }
            for l in &rows {
                println!("{}\t{}\t{}", l.name, l.kind, l.entity);
            }
        }
        eprintln!("{} links on {entity}", rows.len());
        return Ok(());
    }

    let all = meta.field_list(entity);
    let rows: Vec<&meta::FieldInfo<'_>> = match field {
        Some(f) => all.iter().filter(|x| x.name.eq_ignore_ascii_case(f)).collect(),
        None => all.iter().collect(),
    };
    if rows.is_empty() {
        return Err(usage(match field {
            Some(f) => format!("{entity} has no field {f:?}"),
            None => format!("{entity} has no fields in metadata"),
        }));
    }
    if as_json {
        let v: Vec<Value> = rows
            .iter()
            .map(|f| json!({"field": f.name, "type": f.kind, "required": f.required, "options": f.options}))
            .collect();
        output::print_json(&Value::Array(v));
    } else {
        if header {
            println!("field\ttype\treq\toptions");
        }
        for f in &rows {
            let req = if f.required { "*" } else { "" };
            println!("{}\t{}\t{}\t{}", f.name, f.kind, req, f.options.join(","));
        }
    }
    eprintln!("{} fields on {entity}", rows.len());
    Ok(())
}

fn explicit_columns(select: Option<&str>) -> Vec<String> {
    match select {
        None | Some("*") => Vec::new(),
        Some(s) => s.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect(),
    }
}

/// Default is id, name and whatever the caller filtered or sorted on; '*' means no restriction.
fn select_columns(
    meta: &Meta,
    entity: &str,
    select: Option<&str>,
    filters: &[String],
    order: Option<&str>,
) -> Vec<String> {
    if let Some(s) = select {
        return explicit_columns(Some(s));
    }
    let mut cols = vec!["id".to_string()];
    if meta.has_field(entity, "name") {
        cols.push("name".to_string());
    }
    for f in query::where_fields(filters) {
        if !cols.contains(&f) {
            cols.push(f);
        }
    }
    if let Some(o) = order {
        let o = o.to_string();
        if !cols.contains(&o) {
            cols.push(o);
        }
    }
    cols
}

#[allow(clippy::too_many_arguments)]
fn search_params(
    meta: &Meta,
    entity: &str,
    filters: &[String],
    where_json: Option<&str>,
    limit: u32,
    offset: u32,
    order: Option<&str>,
    desc: bool,
    cols: &[String],
) -> Result<Value> {
    let mut clauses = query::parse_where(filters, meta, entity)?;
    if let Some(raw) = where_json {
        let extra: Value =
            serde_json::from_str(raw).map_err(|e| usage(format!("--where-json is not valid JSON: {e}")))?;
        match extra {
            Value::Array(items) => clauses.extend(items),
            Value::Object(_) => clauses.push(extra),
            _ => return Err(usage("--where-json must be a JSON array or object")),
        }
    }
    let mut params = json!({"maxSize": limit, "offset": offset});
    if !cols.is_empty() {
        params["select"] = Value::Array(cols.iter().map(|c| Value::String(c.clone())).collect());
    }
    if !clauses.is_empty() {
        params["where"] = Value::Array(clauses);
    }
    if let Some(o) = order {
        params["orderBy"] = Value::String(o.to_string());
        params["order"] = Value::String(if desc { "desc".into() } else { "asc".to_string() });
    }
    Ok(params)
}

fn list_request(client: &Client, path: &str, params: &Value, no_total: bool) -> Result<Value> {
    let query = [("searchParams", params.to_string())];
    let headers: &[(&str, &str)] = if no_total { &[("X-No-Total", "true")] } else { &[] };
    client.request("GET", path, &query, None, headers)
}

/// Espo adds createdAt, createdById and assignedUserId regardless of select, so project here.
fn emit_list(resp: &Value, cols: &[String], as_json: bool, header: bool, dry_run: bool) {
    if dry_run {
        return;
    }
    if as_json {
        output::print_json(resp);
        return;
    }
    let empty: Vec<Value> = Vec::new();
    let rows = resp.get("list").and_then(Value::as_array).unwrap_or(&empty);
    let cols = output::columns(rows, cols);
    output::print_rows(rows, &cols, header);
    match resp.get("total").and_then(Value::as_i64) {
        Some(t) if t >= 0 => eprintln!("{}/{}", rows.len(), t),
        _ => eprintln!("{}", rows.len()),
    }
}

fn emit_written(resp: &Value, as_json: bool, dry_run: bool) {
    if dry_run {
        return;
    }
    if as_json {
        output::print_json(resp);
        return;
    }
    match resp.get("id").and_then(Value::as_str) {
        Some(id) => println!("{id}"),
        None => output::print_json(resp),
    }
    eprintln!("ok");
}

fn read_json(source: &str) -> Result<Value> {
    let text = if source == "-" {
        std::io::read_to_string(std::io::stdin()).context("reading JSON from stdin")?
    } else {
        source.to_string()
    };
    serde_json::from_str(&text).map_err(|e| usage(format!("invalid JSON body: {e}")))
}

fn build_body(
    meta: &Meta,
    entity: &str,
    fields: &[String],
    data: Option<&str>,
) -> Result<Map<String, Value>> {
    let mut body = match data {
        Some(s) => match read_json(s)? {
            Value::Object(o) => o,
            _ => return Err(usage("--data must be a JSON object")),
        },
        None => Map::new(),
    };
    // field=value pairs win over --data so a stored template can be overridden inline.
    for (k, v) in query::parse_assignments(fields, meta, entity)? {
        body.insert(k, v);
    }
    Ok(body)
}
