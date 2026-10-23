mod app;
mod bulk;
mod cache;
mod camera;
mod config;
mod detect;
mod engine;
mod live;
mod llm;
mod model;
mod movers;
mod moxfield;
mod ocr;
mod pipeline;
mod pricing;
mod printing;
mod recognize;
mod reprice;
mod scryfall;
mod store;
mod tls;
mod web;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};

use app::{App, describe_card, describe_prices};
use config::{Config, LlmProvider, RecognitionMode, StorageBackend};
use engine::EngineOptions;
use llm::VisionLlm;
use model::{CollectionEntry, Finish, format_eur, format_usd};
use mtg_scan_shared::Mode;
use recognize::{CardHints, Method, Recognizer};
use store::{CollectionStore, totals};

#[derive(Parser)]
#[command(
    name = "mtg-scan",
    version,
    about = "The Shuffling Tome: scan Magic: The Gathering cards with a webcam, keep the collection in SQLite or PostgreSQL, and track its prices"
)]
struct Cli {
    /// Settings file (default: $MTG_SCAN_CONFIG, else ./mtg-scan.toml, then
    /// config.toml in the user's config folder)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Collection Markdown file (markdown backend; overrides config)
    #[arg(long, global = true)]
    collection: Option<PathBuf>,
    /// SQLite database file (sqlite backend; overrides [storage] path)
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    /// Where the collection is stored (overrides [storage] backend)
    #[arg(long, global = true, value_enum)]
    storage: Option<StorageBackend>,
    /// How cards are recognised (overrides config)
    #[arg(long, global = true, value_enum)]
    mode: Option<RecognitionMode>,
    /// Vision LLM provider (overrides config)
    #[arg(long, global = true, value_enum)]
    llm_provider: Option<LlmProvider>,
    /// Vision LLM model id (overrides config)
    #[arg(long, global = true)]
    llm_model: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Live scanning of one card at a time inside an on-screen guide.
    /// Scans automatically whenever a new card is held still in the guide.
    Scan {
        #[command(flatten)]
        camera: CameraArgs,
        /// Finish for scanned cards (toggle with F while scanning)
        #[arg(long, value_enum, default_value_t = Finish::Nonfoil)]
        finish: Finish,
        /// Only scan when SPACE is pressed (auto-scan off)
        #[arg(long)]
        manual: bool,
        /// Identify and price only; don't add to the collection
        #[arg(long)]
        dry_run: bool,
    },
    /// Live scanning of every card laid out in view (needs a vision LLM).
    /// Rescans automatically whenever cards are added, removed or swapped.
    ScanAll {
        #[command(flatten)]
        camera: CameraArgs,
        #[arg(long, value_enum, default_value_t = Finish::Nonfoil)]
        finish: Finish,
        /// Capture a single frame, report, and exit (no window)
        #[arg(long)]
        once: bool,
        /// Scan this photo instead of the camera (implies --once)
        #[arg(long)]
        image: Option<PathBuf>,
        /// Only scan when SPACE is pressed (auto-scan off)
        #[arg(long)]
        manual: bool,
        /// Identify and price only; don't add to the collection
        #[arg(long)]
        dry_run: bool,
    },
    /// Web UI: camera, controls and each card as it is saved, in a browser.
    /// Reachable from other computers on the network.
    Ui {
        #[command(flatten)]
        camera: CameraArgs,
        /// Start in table mode (every card in view) instead of single-card mode
        #[arg(long)]
        table: bool,
        /// Finish for scanned cards (default: the Settings page's, else nonfoil)
        #[arg(long, value_enum)]
        finish: Option<Finish>,
        /// Only scan when "Scan now" is pressed (auto-scan off)
        #[arg(long)]
        manual: bool,
        /// Identify and price only; don't add to the collection
        #[arg(long)]
        dry_run: bool,
        /// Address to listen on (0.0.0.0 = every network interface)
        #[arg(long, default_value = "0.0.0.0")]
        bind: std::net::IpAddr,
        #[arg(long, default_value_t = 8787)]
        port: u16,
        /// Don't open a browser on this computer
        #[arg(long)]
        no_open: bool,
    },
    /// Identify card(s) from image files, each image showing a single card
    Identify {
        images: Vec<PathBuf>,
        /// Also add the identified cards to the collection
        #[arg(long)]
        add: bool,
        #[arg(long, value_enum, default_value_t = Finish::Nonfoil)]
        finish: Finish,
    },
    /// Add a card by name (and optionally set / collector number)
    Add {
        #[command(flatten)]
        card: CardArgs,
        #[arg(long, default_value_t = 1)]
        qty: u32,
    },
    /// Remove copies of a card from the collection
    Remove {
        /// Name (or part of it) as it appears in the collection
        name: String,
        #[arg(long)]
        set: Option<String>,
        #[arg(long)]
        number: Option<String>,
        #[arg(long, value_enum)]
        finish: Option<Finish>,
        #[arg(long, default_value_t = 1)]
        qty: u32,
    },
    /// Look up a card's current prices without adding it
    Price {
        #[command(flatten)]
        card: CardArgs,
    },
    /// Refresh the card data from Scryfall and record today's prices for every card
    Reprice,
    /// Show a card's price history
    History {
        /// Name (or part of it) as it appears in the collection
        name: String,
        #[arg(long)]
        set: Option<String>,
        #[arg(long)]
        number: Option<String>,
        #[arg(long, value_enum)]
        finish: Option<Finish>,
    },
    /// Cards that gained or lost the most value (postgres backend)
    Movers {
        /// Compare with prices from this many days ago
        #[arg(long, default_value_t = 7)]
        days: u32,
        /// Cards to list per direction and price source
        #[arg(long, default_value_t = 10)]
        top: usize,
    },
    /// Create (or show) the access token other computers need for `mtg-scan ui`
    UiToken {
        /// Show the current token instead of creating a new one
        #[arg(long)]
        show: bool,
    },
    /// Database management (SQLite or PostgreSQL)
    Db {
        #[command(subcommand)]
        command: DbCommand,
    },
    /// Show the collection and its total value
    List,
    /// List available cameras
    Cameras,
    /// Print raw OCR lines for an image (debugging recognition)
    #[command(hide = true)]
    OcrDump { image: PathBuf },
    /// Find cards in a table photo without the LLM (debugging detection)
    #[command(hide = true)]
    DetectDump {
        image: PathBuf,
        /// Also save the photo with the boxes drawn on it
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Save a single camera frame to a file (to check framing / focus)
    Snapshot {
        #[arg(default_value = "snapshot.jpg")]
        output: PathBuf,
        #[command(flatten)]
        camera: CameraArgs,
    },
}

#[derive(Subcommand)]
enum DbCommand {
    /// Create or upgrade the schema (PostgreSQL: uses the admin / schema-owner account)
    Migrate,
    /// Test the database and show row counts
    Status,
    /// PostgreSQL: create a least-privilege login for day-to-day use and switch .env to it
    SetupAppUser {
        /// Role name to create
        #[arg(long, default_value = "mtg_scan_app")]
        role: String,
    },
    /// Copy a Markdown collection into the database (sets quantities)
    Import {
        /// Markdown file (default: the configured collection file)
        file: Option<PathBuf>,
    },
    /// Copy this collection and its price history into another database
    /// (e.g. PostgreSQL into a SQLite file)
    Copy {
        /// The kind of database to copy into
        #[arg(long, value_enum)]
        to: StorageBackend,
        /// SQLite file to copy into (default: the configured SQLite file)
        #[arg(long)]
        file: Option<PathBuf>,
        /// Copy even if the other database already holds cards (quantities are set, not added)
        #[arg(long)]
        merge: bool,
    },
    /// Copy Scryfall's latest card data (details and prices) into the database
    SyncCards {
        /// Copy it even if this file was copied already
        #[arg(long)]
        force: bool,
    },
    /// Write the database collection out as Markdown
    Export {
        /// Output file (default: the configured collection file)
        file: Option<PathBuf>,
    },
}

#[derive(Args, Clone)]
struct CameraArgs {
    /// Camera index from `mtg-scan cameras` (default: the Insta360)
    #[arg(long)]
    camera: Option<u32>,
    /// Capture width
    #[arg(long)]
    width: Option<u32>,
    /// Capture height
    #[arg(long)]
    height: Option<u32>,
}

#[derive(Args, Clone)]
struct CardArgs {
    /// Card name (fuzzy matched)
    name: String,
    /// Set code, e.g. DMU
    #[arg(long)]
    set: Option<String>,
    /// Collector number within the set
    #[arg(long)]
    number: Option<String>,
    #[arg(long, value_enum, default_value_t = Finish::Nonfoil)]
    finish: Finish,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    config::load_env_files();
    let mut cfg = Config::load(cli.config.as_deref())?;
    if let Some(c) = cli.collection {
        cfg.collection = c;
    }
    if let Some(db) = cli.db {
        cfg.storage.path = Some(db);
    }
    if let Some(s) = cli.storage {
        cfg.storage.backend = s;
    }
    if let Some(m) = cli.mode {
        cfg.recognition.mode = m;
    }
    if let Some(p) = cli.llm_provider {
        cfg.llm.provider = p;
    }
    if let Some(m) = cli.llm_model {
        cfg.llm.model = Some(m);
    }

    match cli.command {
        Command::Scan {
            camera,
            finish,
            manual,
            dry_run,
        } => {
            apply_camera(&mut cfg, &camera, Mode::Single);
            live::run(
                &cfg,
                EngineOptions {
                    mode: Mode::Single,
                    finish,
                    auto: !manual,
                    dry_run,
                    // The desktop window has no review list: save as before.
                    review_uncertain: false,
                    wait_for_camera: false,
                },
            )
        }
        Command::ScanAll {
            camera,
            finish,
            once,
            image,
            manual,
            dry_run,
        } => {
            apply_camera(&mut cfg, &camera, Mode::Table);
            if once || image.is_some() {
                live::scan_table_once(&cfg, image.as_deref(), finish, dry_run)
            } else {
                live::run(
                    &cfg,
                    EngineOptions {
                        mode: Mode::Table,
                        finish,
                        auto: !manual,
                        dry_run,
                        review_uncertain: false,
                        wait_for_camera: false,
                    },
                )
            }
        }
        Command::Ui {
            camera,
            table,
            finish,
            manual,
            dry_run,
            bind,
            port,
            no_open,
        } => {
            // The Settings page's startup choices, unless a flag overrides them.
            let saved = cfg.ui.clone();
            let mut overridden = Vec::new();
            if manual && saved.auto_scan {
                overridden.push("--manual: auto-scan started off".to_string());
            }
            if table && !saved.table_mode {
                overridden.push("--table: started in table mode".to_string());
            }
            if let Some(f) = finish.filter(|f| *f != saved.finish) {
                overridden.push(format!("--finish {f}: new cards start as {f}"));
            }
            let mode = if table || saved.table_mode {
                Mode::Table
            } else {
                Mode::Single
            };
            apply_camera(&mut cfg, &camera, mode);
            web::run(
                &cfg,
                EngineOptions {
                    mode,
                    finish: finish.unwrap_or(saved.finish),
                    auto: saved.auto_scan && !manual,
                    dry_run,
                    review_uncertain: cfg.recognition.review_uncertain,
                    wait_for_camera: true,
                },
                web::WebOptions {
                    addr: std::net::SocketAddr::new(bind, port),
                    open_browser: !no_open,
                    overridden,
                },
            )
        }
        Command::Identify {
            images,
            add,
            finish,
        } => identify(&cfg, &images, add, finish),
        Command::Add { card, qty } => {
            let mut app = App::new(&cfg)?;
            let found = lookup(&app, &card)?;
            let entry = app.add_card(&found, card.finish, qty)?;
            println!("✓ {}", describe_card(&found, entry.finish));
            println!(
                "    {} · now x{}",
                describe_prices(&entry.prices),
                entry.quantity
            );
            Ok(())
        }
        Command::Remove {
            name,
            set,
            number,
            finish,
            qty,
        } => remove(&cfg, &name, set, number, finish, qty),
        Command::Price { card } => {
            let app = App::new(&cfg)?;
            let found = lookup(&app, &card)?;
            let finish = found.best_finish(card.finish);
            let prices = app.prices.price(&found, finish);
            println!("{}", describe_card(&found, finish));
            println!("    {}", describe_prices(&prices));
            for url in [&prices.tcgplayer_url, &prices.cardkingdom_url]
                .into_iter()
                .flatten()
            {
                println!("    {url}");
            }
            Ok(())
        }
        Command::Reprice => reprice(&cfg),
        Command::History {
            name,
            set,
            number,
            finish,
        } => history(&cfg, &name, set, number, finish),
        Command::Movers { days, top } => movers(&cfg, days, top),
        Command::UiToken { show } => ui_token(show),
        Command::Db { command } => db(&cfg, command),
        Command::List => list(&cfg),
        Command::Cameras => {
            for c in camera::list_cameras()? {
                println!("{}: {}", c.index(), c.human_name());
            }
            Ok(())
        }
        Command::DetectDump { image, out } => {
            let img = image::open(&image)
                .with_context(|| format!("failed to open {}", image.display()))?
                .to_rgb8();
            let started = std::time::Instant::now();
            let analysis = detect::analyse(&img);
            let boxes = analysis.boxes;
            println!(
                "{} card(s) in {} ms",
                boxes.len(),
                started.elapsed().as_millis()
            );
            let (w, h) = img.dimensions();
            let mut marked = img.clone();
            for b in &boxes {
                let r = pipeline::Rect::from_norm(*b, w, h, 0.0);
                println!("  x={:<5} y={:<5} {}x{}", r.x, r.y, r.w, r.h);
                engine::draw::outline(&mut marked, r, (w / 600).max(2), [40, 220, 120]);
            }
            if let Some(out) = out {
                marked
                    .save(&out)
                    .with_context(|| format!("failed to write {}", out.display()))?;
                let edges = out.with_extension("edges.png");
                analysis.debug.save(&edges)?;
                println!("saved {} and {}", out.display(), edges.display());
            }
            Ok(())
        }
        Command::OcrDump { image } => {
            let img = image::open(&image)
                .with_context(|| format!("failed to open {}", image.display()))?;
            for l in ocr::Ocr::new()?.recognize(&img)? {
                println!(
                    "[x {:.3} y {:.3} w {:.3} h {:.4}] {}",
                    l.x, l.y, l.w, l.h, l.text
                );
            }
            Ok(())
        }
        Command::Snapshot { output, camera } => {
            apply_camera(&mut cfg, &camera, Mode::Single);
            let mut cam =
                camera::Webcam::open(cfg.camera.index, cfg.camera.width, cfg.camera.height)?;
            let frame = camera::warm_frame(&mut cam)?;
            frame
                .save(&output)
                .with_context(|| format!("failed to save {}", output.display()))?;
            println!(
                "Saved {}x{} frame from {} to {}",
                frame.width(),
                frame.height(),
                cam.name,
                output.display()
            );
            Ok(())
        }
    }
}

/// Apply `--camera/--width/--height`; the size applies to the given mode.
fn apply_camera(cfg: &mut Config, args: &CameraArgs, mode: Mode) {
    if args.camera.is_some() {
        cfg.camera.index = args.camera;
    }
    let (w, h) = match mode {
        Mode::Single => (&mut cfg.camera.width, &mut cfg.camera.height),
        Mode::Table => (&mut cfg.camera.table_width, &mut cfg.camera.table_height),
    };
    if let Some(v) = args.width {
        *w = v;
    }
    if let Some(v) = args.height {
        *h = v;
    }
}

fn lookup(app: &App, args: &CardArgs) -> Result<model::Card> {
    let hints = CardHints {
        name: Some(args.name.clone()),
        set_code: args.set.as_ref().map(|s| s.to_lowercase()),
        collector_number: args
            .number
            .as_deref()
            .map(recognize::normalize_collector_number),
        foil: None,
    };
    let names = recognize::NameIndex::load(&app.scryfall);
    match recognize::resolve(&hints, &app.scryfall, Method::Ocr, &names)? {
        Some(id) => Ok(id.card),
        None => bail!("no card found matching {hints}"),
    }
}

fn identify(cfg: &Config, images: &[PathBuf], add: bool, finish: Finish) -> Result<()> {
    if images.is_empty() {
        bail!("pass at least one image file");
    }
    let mut app = App::new(cfg)?;
    let llm = VisionLlm::from_config(&cfg.llm)?;
    let recognizer = Recognizer::new(cfg.recognition.mode, llm, &app.scryfall)?;
    for path in images {
        let img =
            image::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        print!("{}: ", path.display());
        match recognizer.identify(&img, &app.scryfall, false) {
            Ok(id) => {
                let f = id.card.best_finish(finish);
                println!(
                    "{} via {} ({})",
                    describe_card(&id.card, f),
                    id.method,
                    id.quality.label()
                );
                if add {
                    let entry = app.add_card(&id.card, finish, 1)?;
                    println!(
                        "    added · {} · now x{}",
                        describe_prices(&entry.prices),
                        entry.quantity
                    );
                }
            }
            Err(e) => println!("✗ {e:#}"),
        }
    }
    Ok(())
}

fn remove(
    cfg: &Config,
    name: &str,
    set: Option<String>,
    number: Option<String>,
    finish: Option<Finish>,
    qty: u32,
) -> Result<()> {
    let mut app = App::new(cfg)?;
    let e = find_one(&app, name, set, number, finish)?;
    let left = app
        .store
        .remove(&e.scryfall_id, e.finish, qty)?
        .map(|e| e.quantity)
        .unwrap_or(0);
    println!(
        "Removed {} x {} ({} #{}, {}); {left} left",
        qty.min(e.quantity),
        e.name,
        e.set_code,
        e.collector_number,
        e.finish
    );
    Ok(())
}

/// The single collection row matching a name (+ optional set / number /
/// finish). Lists the candidates when it's ambiguous.
fn find_one(
    app: &App,
    name: &str,
    set: Option<String>,
    number: Option<String>,
    finish: Option<Finish>,
) -> Result<CollectionEntry> {
    let needle = name.to_lowercase();
    let matches: Vec<CollectionEntry> = app
        .store
        .entries()?
        .into_iter()
        .filter(|e| e.name.to_lowercase().contains(&needle))
        .filter(|e| {
            set.as_ref()
                .is_none_or(|s| e.set_code.eq_ignore_ascii_case(s))
        })
        .filter(|e| {
            number
                .as_ref()
                .is_none_or(|n| e.collector_number.eq_ignore_ascii_case(n))
        })
        .filter(|e| finish.is_none_or(|f| e.finish == f))
        .collect();
    match matches.as_slice() {
        [] => bail!("no card in the collection matches '{name}'"),
        [e] => Ok(e.clone()),
        many => {
            println!("'{name}' matches several rows; narrow it with --set / --number / --finish:");
            for e in many {
                println!(
                    "  {} ({} #{}, {}) x{}",
                    e.name, e.set_code, e.collector_number, e.finish, e.quantity
                );
            }
            bail!("ambiguous card")
        }
    }
}

fn history(
    cfg: &Config,
    name: &str,
    set: Option<String>,
    number: Option<String>,
    finish: Option<Finish>,
) -> Result<()> {
    let app = App::new(cfg)?;
    let e = find_one(&app, name, set, number, finish)?;
    let points = app.store.price_history(&e.scryfall_id, e.finish)?;
    println!(
        "{} ({} #{}, {}) x{}",
        e.name, e.set_code, e.collector_number, e.finish, e.quantity
    );
    let ck = points.iter().any(|p| p.cardkingdom.is_some());
    print!("{:<10}  {:>9}  {:>9}", "Date", "TCGplayer", "Cardmarket");
    println!("{}", if ck { "         CK     CK buy" } else { "" });
    for p in &points {
        print!(
            "{:<10}  {:>9}  {:>10}",
            p.date.to_string(),
            format_usd(p.tcgplayer),
            format_eur(p.cardmarket)
        );
        if ck {
            print!(
                "  {:>9}  {:>9}",
                format_usd(p.cardkingdom),
                format_usd(p.cardkingdom_buylist)
            );
        }
        println!();
    }
    if let (Some(first), Some(last)) = (points.first(), points.last())
        && let (Some(a), Some(b)) = (first.tcgplayer, last.tcgplayer)
        && points.len() > 1
    {
        let pct = if a > 0.0 { (b - a) / a * 100.0 } else { 0.0 };
        println!(
            "TCGplayer change since {}: {:+.2} ({pct:+.1}%)",
            first.date,
            b - a
        );
    }
    Ok(())
}

fn movers(cfg: &Config, days: u32, top: usize) -> Result<()> {
    let app = App::new(cfg)?;
    let since = movers::since(chrono::Local::now().date_naive(), days);
    let moves = app.store.price_moves(since)?;
    if moves.is_empty() {
        println!(
            "No card owned now was priced on or before {since} and again since. Price history \
             grows by a point a day while mtg-scan runs (or with `mtg-scan reprice`)."
        );
        return Ok(());
    }
    let entries = app.store.entries()?;
    let describe = |m: &store::PriceMove| {
        entries
            .iter()
            .find(|e| e.scryfall_id == m.scryfall_id && e.finish == m.finish)
            .map(|e| {
                format!(
                    "{} ({} #{}, {})",
                    e.name, e.set_code, e.collector_number, e.finish
                )
            })
            .unwrap_or_else(|| format!("{} ({})", m.scryfall_id, m.finish))
    };
    let pct = |p: Option<f64>| p.map(|p| format!(" ({p:+.1}%)")).unwrap_or_default();
    let signed = |source: movers::Source, v: f64| {
        format!(
            "{}{}",
            if v < 0.0 { "-" } else { "+" },
            source.money(v.abs())
        )
    };

    println!("Since {since} ({days} days):");
    let report = movers::report(&moves, top);
    // Sources with no prices on both dates (e.g. Card Kingdom switched off) are left out.
    let report: Vec<_> = report.into_iter().filter(|r| r.rows > 0).collect();
    for r in &report {
        println!(
            "  {:<13} {} → {}  {}{}  over {} printings",
            r.source.label(),
            r.source.money(r.before),
            r.source.money(r.now),
            signed(r.source, r.now - r.before),
            pct(r.percent()),
            r.rows
        );
    }
    for r in &report {
        for (title, list) in [("gains", &r.risers), ("drops", &r.fallers)] {
            if list.is_empty() {
                continue;
            }
            println!("\n{} — biggest {title}", r.source.label());
            for c in list {
                let m = &moves[c.row];
                println!(
                    "  {:>9}  {}{}  {} → {}{}",
                    signed(r.source, c.total()),
                    describe(m),
                    if c.quantity > 1 {
                        format!(" ×{}", c.quantity)
                    } else {
                        String::new()
                    },
                    r.source.money(c.then),
                    r.source.money(c.now),
                    pct(c.percent())
                );
            }
        }
    }
    Ok(())
}

/// Save a new web UI access token in `.env` (replacing any old one, which
/// signs other computers out), or show the current one.
fn ui_token(show: bool) -> Result<()> {
    const VAR: &str = "MTG_SCAN_UI_TOKEN";
    let token = if show {
        std::env::var(VAR)
            .ok()
            .filter(|t| !t.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("no {VAR} yet; run `mtg-scan ui-token` to create one"))?
    } else {
        let env_file = std::path::Path::new(".env");
        if !env_file.is_file() {
            bail!("run this from the folder containing your .env file");
        }
        let token = generate_password()?;
        config::update_env_file(env_file, &[(VAR, Some(&token))])?;
        println!("Saved a new {VAR} in .env. Restart `mtg-scan ui` to use it.");
        token
    };
    println!("Access token: {token}");
    println!(
        "On another computer, open http://<this computer>:8787/?token={token} once, or enter the token on the sign-in page."
    );
    Ok(())
}

fn db(cfg: &Config, command: DbCommand) -> Result<()> {
    use store::postgres::PostgresStore;
    use store::sqlite::SqliteStore;
    let scryfall = std::rc::Rc::new(scryfall::Scryfall::new(app::http_client()?));
    let schema = cfg.storage.schema.as_str();
    match (command, cfg.storage.backend) {
        (_, StorageBackend::Markdown) => bail!(
            "`db` commands need a database: set [storage] backend = \"sqlite\" (the default)              or \"postgres\", or pass --storage sqlite"
        ),
        (DbCommand::Migrate, StorageBackend::Sqlite) => {
            // Opening the file applies pending migrations.
            let store = SqliteStore::open(cfg.storage.sqlite_path(), None)?;
            println!(
                "{}: schema is up to date (version {})",
                store.location(),
                store.schema_version()?
            );
        }
        (DbCommand::Migrate, StorageBackend::Postgres) => {
            let admin = cfg.storage.resolved_admin_connection()?;
            let (store, applied) = PostgresStore::connect_admin(admin, schema, scryfall)?;
            if applied.is_empty() {
                println!(
                    "{}: schema is up to date (version {})",
                    store.location(),
                    store.schema_version()?
                );
            } else {
                println!("{}: applied migrations {applied:?}", store.location());
            }
        }
        (DbCommand::Status, StorageBackend::Sqlite) => {
            let store = SqliteStore::open(cfg.storage.sqlite_path(), None)?;
            let s = store.status()?;
            println!("Opened {}", store.location());
            print_cache(&store);
            println!("  engine           SQLite {}", s.sqlite_version);
            if let Ok(meta) = std::fs::metadata(store.path()) {
                println!(
                    "  size             {:.1} MB",
                    meta.len() as f64 / 1_048_576.0
                );
            }
            print_counts(
                s.schema_version,
                s.printings,
                s.cards,
                s.price_points,
                s.first_price_date,
            );
        }
        (DbCommand::Status, StorageBackend::Postgres) => {
            // No version check here, so status works even when migrations are pending.
            let store = PostgresStore::open(cfg.storage.resolved_connection()?, schema, scryfall)?;
            let s = store.status()?;
            println!("Connected to {}", store.location());
            if s.schema_version >= 2 {
                print_cache(&store);
            }
            println!("  server           PostgreSQL {}", s.server_version);
            println!(
                "  logged in as     {}{}",
                s.user,
                if s.can_create_roles {
                    " (can create roles)"
                } else {
                    ""
                }
            );
            print_counts(
                s.schema_version,
                s.printings,
                s.cards,
                s.price_points,
                s.first_price_date,
            );
        }
        (DbCommand::SetupAppUser { .. }, StorageBackend::Sqlite) => {
            bail!("setup-app-user is for PostgreSQL; a SQLite file has no logins")
        }
        (DbCommand::SetupAppUser { role }, StorageBackend::Postgres) => {
            setup_app_user(cfg, &role, scryfall)?
        }
        (DbCommand::Import { file }, _) => {
            let mut store = App::new(cfg)?.store;
            let path = file.unwrap_or_else(|| cfg.collection.clone());
            let entries = store::markdown::MarkdownStore::open(&path)?.entries()?;
            let n = store.import(&entries)?;
            println!(
                "Imported {n} rows from {} into {}",
                path.display(),
                store.location()
            );
        }
        (DbCommand::Copy { to, file, merge }, from) => {
            let mut target = cfg.clone();
            target.storage.backend = to;
            if let Some(file) = file {
                target.storage.path = Some(file);
            }
            if to == StorageBackend::Markdown {
                bail!("copy into sqlite or postgres; `db export` writes a Markdown file");
            }
            if to == from && target.storage.sqlite_path() == cfg.storage.sqlite_path() {
                bail!("that is the database already in use: pass --to and, for SQLite, --file");
            }
            // Copying only reads, so a PostgreSQL source may be an older schema
            // (e.g. a 0.x database still used by a 0.x install): it isn't upgraded.
            let source: Box<dyn CollectionStore> = match from {
                StorageBackend::Postgres => {
                    let store = PostgresStore::open(
                        cfg.storage.resolved_connection()?,
                        schema,
                        scryfall.clone(),
                    )?;
                    let (version, latest) =
                        (store.schema_version()?, store::postgres::latest_migration());
                    if version > latest {
                        bail!(
                            "{} is at schema version {version}, newer than this mtg-scan supports \
                             ({latest}): update mtg-scan",
                            store.location()
                        );
                    }
                    Box::new(store)
                }
                _ => App::new(cfg)?.store,
            };
            let mut dest = App::new(&target)?;
            let existing = dest.store.entries()?.len();
            if existing > 0 && !merge {
                bail!(
                    "{} already holds {existing} rows; --merge copies anyway (quantities are set \
                     to this collection's, other rows are kept)",
                    dest.store.location()
                );
            }
            println!("Copying {} → {}…", source.location(), dest.store.location());
            let (rows, points) = store::copy(source.as_ref(), dest.store.as_mut())?;
            println!(
                "Copied {rows} rows and {points} price points. Card data rebuilds itself there \
                 (or run db sync-cards against it)."
            );
        }
        (DbCommand::SyncCards { force }, _) => {
            let app = App::new(cfg)?;
            let cache = app
                .store
                .card_cache()
                .context("this storage backend has no card cache")?;
            println!("Checking Scryfall's bulk data…");
            let r = bulk::sync(&app.scryfall, cache, force)?;
            if r.skipped {
                println!(
                    "Already up to date (Scryfall's file of {}); --force copies it again",
                    r.file_date
                );
            } else {
                println!(
                    "Copied {} printings from Scryfall's file of {}{}",
                    r.cards,
                    r.file_date,
                    if r.unreadable > 0 {
                        format!(" ({} lines skipped)", r.unreadable)
                    } else {
                        String::new()
                    }
                );
            }
        }
        (DbCommand::Export { file }, _) => {
            let store = App::new(cfg)?.store;
            let path = file.unwrap_or_else(|| cfg.collection.clone());
            let entries = store.entries()?;
            std::fs::write(&path, store::markdown::render(&entries))
                .with_context(|| format!("failed to write {}", path.display()))?;
            println!("Exported {} rows to {}", entries.len(), path.display());
        }
    }
    Ok(())
}

/// The card cache's size and when it was last synced.
fn print_cache(store: &dyn store::CardCache) {
    let synced = store
        .job(bulk::JOB)
        .ok()
        .flatten()
        .map(|j| {
            format!(
                ", synced {}",
                j.done_at
                    .with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
            )
        })
        .unwrap_or_else(|| ", not synced yet (db sync-cards)".into());
    match store.cached_count() {
        Ok(n) => println!("  card cache       {n} printings{synced}"),
        Err(e) => println!("  card cache       unavailable ({e:#})"),
    }
}

fn print_counts(
    schema_version: i32,
    printings: i64,
    cards: i64,
    price_points: i64,
    first_price_date: Option<chrono::NaiveDate>,
) {
    println!("  schema version   {schema_version}");
    println!("  printings        {printings}");
    println!("  cards            {cards}");
    println!(
        "  price points     {price_points}{}",
        first_price_date
            .map(|d| format!(" (since {d})"))
            .unwrap_or_default()
    );
}

/// Create the least-privilege app role and move `.env` over to it, keeping
/// the schema owner's login as MTG_SCAN_DB_ADMIN_*. If the owner may not
/// create roles, print the one statement a superuser must run and stop; the
/// generated password waits in `.env` until this command is run again.
fn setup_app_user(
    cfg: &Config,
    role: &str,
    scryfall: std::rc::Rc<scryfall::Scryfall>,
) -> Result<()> {
    use store::postgres::{PostgresStore, RoleSetup};
    const PENDING: &str = "MTG_SCAN_DB_APP_PASSWORD_PENDING";
    let env_file = std::path::Path::new(".env");
    if !env_file.is_file() {
        bail!("run this from the folder containing your .env file");
    }
    if std::env::var_os("MTG_SCAN_DATABASE_URL").is_some() {
        bail!(
            "setup-app-user works with the MTG_SCAN_DB_* settings; remove MTG_SCAN_DATABASE_URL from .env first"
        );
    }
    let admin = cfg.storage.resolved_admin_connection()?;
    let owner = admin.get_user().unwrap_or_default().to_string();
    if owner == role {
        bail!(
            "the admin login is already '{role}': set MTG_SCAN_DB_ADMIN_USER / _ADMIN_PASSWORD to the schema owner"
        );
    }
    let password = match std::env::var(PENDING) {
        Ok(p) if !p.is_empty() => p,
        _ => generate_password()?,
    };
    let (store, _) =
        PostgresStore::connect_admin(admin.clone(), &cfg.storage.schema, scryfall.clone())?;
    match store.setup_app_role(role, &password)? {
        RoleSetup::NeedsSuperuser { sql } => {
            config::update_env_file(env_file, &[(PENDING, Some(&password))])?;
            println!(
                "'{owner}' is not allowed to create logins, so a PostgreSQL superuser (e.g. 'postgres')"
            );
            println!(
                "needs to run this one statement (it contains a password hash, not the password):\n"
            );
            println!("    {sql}\n");
            println!(
                "Then run `mtg-scan db setup-app-user` again to finish. The new password is saved in .env."
            );
        }
        RoleSetup::Ready { created } => {
            let mut app_conn = admin.clone();
            app_conn.user(role).password(&password);
            PostgresStore::connect(app_conn, &cfg.storage.schema, scryfall)
                .with_context(|| format!("logging in as '{role}' failed; was it created with the statement printed earlier?"))?;
            let owner_password =
                String::from_utf8_lossy(admin.get_password().unwrap_or_default()).to_string();
            let mut updates: Vec<(&str, Option<&str>)> = vec![
                ("MTG_SCAN_DB_USER", Some(role)),
                ("MTG_SCAN_DB_PASSWORD", Some(&password)),
                (PENDING, None),
            ];
            if std::env::var_os("MTG_SCAN_DB_ADMIN_USER").is_none() {
                updates.push(("MTG_SCAN_DB_ADMIN_USER", Some(&owner)));
                updates.push(("MTG_SCAN_DB_ADMIN_PASSWORD", Some(&owner_password)));
            }
            config::update_env_file(env_file, &updates)?;
            println!(
                "{} role '{role}' with read/write access to rows in schema '{}' (no schema changes).",
                if created { "Created" } else { "Configured" },
                cfg.storage.schema
            );
            println!(
                "Updated .env: the app now logs in as '{role}'; '{owner}' is kept as MTG_SCAN_DB_ADMIN_USER"
            );
            println!("for `db migrate` and future role changes.");
        }
    }
    Ok(())
}

/// 32 random alphanumeric characters (unbiased, from the OS RNG).
fn generate_password() -> Result<String> {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::new();
    while out.len() < 32 {
        let mut buf = [0u8; 64];
        getrandom::fill(&mut buf).map_err(|e| anyhow::anyhow!("no secure random source: {e}"))?;
        // Reject bytes >= 248 (= 62 * 4) so every character is equally likely.
        out.extend(
            buf.iter()
                .filter(|b| **b < 248)
                .map(|b| CHARS[(*b % 62) as usize] as char)
                .take(32 - out.len()),
        );
    }
    Ok(out)
}

fn reprice(cfg: &Config) -> Result<()> {
    let mut app = App::new(cfg)?;
    let cache = app
        .store
        .card_cache()
        .context("this storage backend has no card data")?;
    println!("Checking Scryfall's card data…");
    let synced = bulk::sync(&app.scryfall, cache, false)?;
    if !synced.skipped {
        println!("Copied {} printings from Scryfall", synced.cards);
    }
    if let Some(ck) = &app.prices.cardkingdom {
        ck.force_refresh()
            .context("failed to refresh Card Kingdom prices")?;
    }
    let refreshed = reprice::refresh(&mut app, true)?.unwrap_or(reprice::Refreshed {
        rows: 0,
        card_kingdom: 0,
    });
    println!(
        "Recorded today's prices for {} printings{}",
        refreshed.rows,
        if app.prices.cardkingdom.is_some() {
            format!(" ({} with Card Kingdom prices)", refreshed.card_kingdom)
        } else {
            String::new()
        }
    );
    print_totals(&app.store.entries()?);
    Ok(())
}

fn list(cfg: &Config) -> Result<()> {
    let app = App::new(cfg)?;
    let entries = app.store.entries()?;
    if entries.is_empty() {
        println!("{} is empty — scan some cards first.", app.store.location());
        return Ok(());
    }
    let name_w = entries
        .iter()
        .map(|e| e.name.chars().count())
        .max()
        .unwrap_or(4)
        .clamp(4, 40);
    let ck = entries.iter().any(|e| e.prices.cardkingdom.is_some());
    print!(
        "{:>3}  {:<name_w$}  {:<5} {:>5}  {:<7}  {:>9}  {:>10}",
        "Qty", "Name", "Set", "#", "Finish", "TCGplayer", "Cardmarket"
    );
    println!("{}", if ck { "         CK     CK buy" } else { "" });
    for e in &entries {
        let name: String = e.name.chars().take(name_w).collect();
        print!(
            "{:>3}  {:<name_w$}  {:<5} {:>5}  {:<7}  {:>9}  {:>10}",
            e.quantity,
            name,
            e.set_code,
            e.collector_number,
            e.finish.as_str(),
            format_usd(e.prices.tcgplayer),
            format_eur(e.prices.cardmarket)
        );
        if ck {
            print!(
                "  {:>9}  {:>9}",
                format_usd(e.prices.cardkingdom),
                format_usd(e.prices.cardkingdom_buylist)
            );
        }
        println!();
    }
    print_totals(&entries);
    Ok(())
}

fn print_totals(entries: &[CollectionEntry]) {
    let t = totals(entries);
    print!(
        "\n{} printings, {} cards · TCGplayer ${:.2} · Cardmarket €{:.2}",
        t.printings, t.cards, t.tcgplayer, t.cardmarket
    );
    if entries.iter().any(|e| e.prices.cardkingdom.is_some()) {
        print!(
            " · Card Kingdom ${:.2} (buylist ${:.2})",
            t.cardkingdom, t.cardkingdom_buylist
        );
    }
    println!();
}
