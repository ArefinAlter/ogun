//! Adapters around single external tools, and the `shell` code operator.
//!
//! Arguments are built from the closed configuration of each operator; nothing a configuration holds is
//! ever interpreted by a shell. Secrets reach a tool only through its environment, inside the sandbox.

use std::io::BufRead;
use std::path::PathBuf;
use std::process::Command;

use serde_json::{Value, json};

use crate::proto::{
    self, Input, Shim, ShimError, emit_dir, find, output, progress, safe_rel, sanitize,
};

fn is_pdf(i: &Input) -> bool {
    i.media_type == "application/pdf" || i.name().to_ascii_lowercase().ends_with(".pdf")
}

/// `shell@1 <code>`: runs `sh <code> <input paths...>` in scratch. The script sees `OGUN_INPUTS` (a JSON
/// file with the inputs), `OGUN_CONFIG` (the configuration without secret values), `OGUN_OUT` (write
/// outputs there) and one `OGUN_SECRET_<NAME>` per secret the node references. Files in `out/` become
/// `artifacts`.
pub fn shell(shim: &Shim, code: Option<&str>) -> Result<(), ShimError> {
    let code = code.ok_or_else(|| ShimError::config("shell needs its verified script"))?;
    let sh = find(&["sh"])?;
    let work = shim.dir(".ogun")?;
    let out = shim.dir("out")?;
    let inputs: Vec<Value> = shim
        .inputs
        .iter()
        .map(|i| json!({"port": i.port, "path": i.path, "blake3": i.blake3, "media_type": i.media_type, "metadata": i.metadata}))
        .collect();
    std::fs::write(work.join("inputs.json"), Value::Array(inputs).to_string())
        .map_err(|e| ShimError::io("writing inputs.json", &e))?;
    std::fs::write(
        work.join("config.json"),
        Value::Object(shim.config().clone()).to_string(),
    )
    .map_err(|e| ShimError::io("writing config.json", &e))?;
    let mut cmd = Command::new(sh);
    cmd.arg(code)
        .args(shim.inputs.iter().map(|i| &i.path))
        .current_dir(&shim.scratch)
        .env("OGUN_INPUTS", work.join("inputs.json"))
        .env("OGUN_CONFIG", work.join("config.json"))
        .env("OGUN_OUT", &out);
    for (key, value) in shim.config() {
        let is_ref = value
            .as_object()
            .is_some_and(|o| o.contains_key("secret_ref"));
        if is_ref && let Some(text) = shim.text_of(value) {
            let var: String = format!("OGUN_SECRET_{}", key.to_ascii_uppercase())
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            cmd.env(var, text.as_str());
        }
    }
    proto::run(&mut cmd, "sh")?;
    let n = emit_dir(&out, "artifacts");
    proto::log(&format!("shell: {n} files in out/"));
    Ok(())
}

/// `pdf.extract@1`: `{"layout", "first_page", "last_page", "min_text_chars"}`. One `text` output per PDF
/// with metadata `pdf.has_text` and `pdf.pages`. A PDF with fewer than `min_text_chars` (default 16)
/// non-space characters has no usable text layer: its `text` output is the PDF itself, with
/// `pdf.has_text: false`, so a route on that field can send it to OCR.
pub fn pdf_extract(shim: &Shim) -> Result<(), ShimError> {
    let pdftotext = find(&["pdftotext"])?;
    let pdfinfo = find(&["pdfinfo"]).ok();
    let dir = shim.dir("text")?;
    let min_chars = usize::try_from(shim.u64("min_text_chars")?.unwrap_or(16)).unwrap_or(16);
    let total = shim.inputs.len() as u64;
    for (n, input) in shim.inputs.iter().enumerate() {
        if !is_pdf(input) {
            return Err(ShimError::input(format!(
                "pdf.extract: input {} is not a PDF",
                n + 1
            )));
        }
        let out = dir.join(format!("{:04}-{}.txt", n + 1, sanitize(&input.stem())));
        let mut cmd = Command::new(&pdftotext);
        cmd.arg("-enc").arg("UTF-8");
        if shim.bool("layout")? == Some(true) {
            cmd.arg("-layout");
        }
        if let Some(f) = shim.u64("first_page")? {
            cmd.arg("-f").arg(f.to_string());
        }
        if let Some(l) = shim.u64("last_page")? {
            cmd.arg("-l").arg(l.to_string());
        }
        cmd.arg(&input.path).arg(&out);
        proto::run(&mut cmd, "pdftotext")?;
        let text = std::fs::read_to_string(&out).unwrap_or_default();
        let chars = text.chars().filter(|c| !c.is_whitespace()).count();
        let pages = pdfinfo.as_ref().and_then(|exe| {
            let info = proto::run_capture(Command::new(exe).arg(&input.path), "pdfinfo").ok()?;
            String::from_utf8_lossy(&info)
                .lines()
                .find_map(|l| {
                    l.strip_prefix("Pages:")
                        .map(|v| v.trim().parse::<u64>().ok())
                })
                .flatten()
        });
        let has_text = chars >= min_chars;
        let md = json!({
            "pdf": {"has_text": has_text, "pages": pages, "text_chars": chars},
            "source_blake3": input.blake3,
            "filename": input.name(),
        });
        if has_text {
            output("text", &out, "text/plain", md);
        } else {
            let _ = std::fs::remove_file(&out);
            let copy = dir.join(format!("{:04}-{}", n + 1, sanitize(&input.name())));
            std::fs::copy(&input.path, &copy)
                .map_err(|e| ShimError::io("copying a PDF without text", &e))?;
            output("text", &copy, "application/pdf", md);
        }
        progress(n as u64 + 1, total);
    }
    Ok(())
}

/// `tesseract@1`: `{"lang", "psm", "dpi"}`. Images are read directly; PDFs are rasterised with `pdftoppm`.
pub fn tesseract(shim: &Shim) -> Result<(), ShimError> {
    let exe = find(&["tesseract"])?;
    let lang = shim.str("lang")?.unwrap_or("eng").to_owned();
    let psm = shim.u64("psm")?;
    let dpi = shim.u64("dpi")?.unwrap_or(300);
    let dir = shim.dir("text")?;
    let total = shim.inputs.len() as u64;
    for (n, input) in shim.inputs.iter().enumerate() {
        let images: Vec<PathBuf> = if is_pdf(input) {
            let pdftoppm = find(&["pdftoppm"])?;
            let pages = shim.dir(&format!(".ogun/pages-{:04}", n + 1))?;
            proto::run(
                Command::new(pdftoppm)
                    .arg("-r")
                    .arg(dpi.to_string())
                    .arg("-png")
                    .arg(&input.path)
                    .arg(pages.join("p")),
                "pdftoppm",
            )?;
            proto::files_under(&pages)
        } else {
            vec![input.path.clone()]
        };
        let mut text = String::new();
        for (k, image) in images.iter().enumerate() {
            let base = shim.dir(".ogun")?.join(format!("ocr-{:04}-{k:05}", n + 1));
            let mut cmd = Command::new(&exe);
            cmd.arg(image)
                .arg(&base)
                .arg("-l")
                .arg(&lang)
                .arg("--dpi")
                .arg(dpi.to_string());
            if let Some(p) = psm {
                cmd.arg("--psm").arg(p.to_string());
            }
            proto::run(&mut cmd, "tesseract")?;
            text.push_str(&std::fs::read_to_string(base.with_extension("txt")).unwrap_or_default());
            text.push('\u{c}');
        }
        let out = dir.join(format!("{:04}-{}.txt", n + 1, sanitize(&input.stem())));
        std::fs::write(&out, &text).map_err(|e| ShimError::io("writing OCR text", &e))?;
        output(
            "text",
            &out,
            "text/plain",
            json!({"ocr": {"engine": "tesseract", "lang": lang, "pages": images.len()}, "source_blake3": input.blake3, "filename": input.name()}),
        );
        progress(n as u64 + 1, total);
    }
    Ok(())
}

fn collect_rec_texts(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            if let Some(Value::Array(texts)) = m.get("rec_texts") {
                out.extend(texts.iter().filter_map(Value::as_str).map(str::to_owned));
            }
            for (k, child) in m {
                if k != "rec_texts" {
                    collect_rec_texts(child, out);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|i| collect_rec_texts(i, out)),
        _ => {}
    }
}

/// `paddleocr@1`: `{"lang"}`. Runs `paddleocr ocr -i <input> --save_path <dir>` (PaddleOCR 3) and joins
/// the recognised texts of the JSON results.
pub fn paddleocr(shim: &Shim) -> Result<(), ShimError> {
    let exe = find(&["paddleocr"])?;
    let dir = shim.dir("text")?;
    for (n, input) in shim.inputs.iter().enumerate() {
        let save = shim.dir(&format!(".ogun/paddle-{:04}", n + 1))?;
        let mut cmd = Command::new(&exe);
        cmd.arg("ocr")
            .arg("-i")
            .arg(&input.path)
            .arg("--save_path")
            .arg(&save);
        if let Some(lang) = shim.str("lang")? {
            cmd.arg("--lang").arg(lang);
        }
        let stdout = proto::run_capture(&mut cmd, "paddleocr")?;
        let mut texts = Vec::new();
        for f in proto::files_under(&save)
            .into_iter()
            .filter(|f| f.extension().is_some_and(|e| e == "json"))
        {
            if let Ok(v) = std::fs::read_to_string(&f)
                .map(|t| serde_json::from_str::<Value>(&t).unwrap_or(Value::Null))
            {
                collect_rec_texts(&v, &mut texts);
            }
        }
        let text = if texts.is_empty() {
            String::from_utf8_lossy(&stdout).into_owned()
        } else {
            texts.join("\n")
        };
        let out = dir.join(format!("{:04}-{}.txt", n + 1, sanitize(&input.stem())));
        std::fs::write(&out, text).map_err(|e| ShimError::io("writing OCR text", &e))?;
        output(
            "text",
            &out,
            "text/plain",
            json!({"ocr": {"engine": "paddleocr"}, "source_blake3": input.blake3, "filename": input.name()}),
        );
    }
    Ok(())
}

/// `ffmpeg@1`: `{"format", "audio_only", "sample_rate", "channels", "max_seconds"}`.
pub fn ffmpeg(shim: &Shim) -> Result<(), ShimError> {
    let exe = find(&["ffmpeg"])?;
    let audio_only = shim.bool("audio_only")?.unwrap_or(false);
    let format = shim
        .str("format")?
        .unwrap_or(if audio_only { "wav" } else { "mp4" })
        .to_owned();
    let allowed = [
        "mp3", "wav", "flac", "ogg", "opus", "m4a", "mp4", "webm", "mkv", "png", "jpg",
    ];
    if !allowed.contains(&format.as_str()) {
        return Err(ShimError::config("ffmpeg: format is not in the closed set"));
    }
    let dir = shim.dir("artifacts")?;
    let total = shim.inputs.len() as u64;
    for (n, input) in shim.inputs.iter().enumerate() {
        let out = dir.join(format!("{:04}-{}.{format}", n + 1, sanitize(&input.stem())));
        let mut cmd = Command::new(&exe);
        cmd.args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(&input.path);
        if audio_only {
            cmd.arg("-vn");
        }
        if let Some(r) = shim.u64("sample_rate")? {
            cmd.arg("-ar").arg(r.to_string());
        }
        if let Some(c) = shim.u64("channels")? {
            cmd.arg("-ac").arg(c.to_string());
        }
        if let Some(t) = shim.u64("max_seconds")? {
            cmd.arg("-t").arg(t.to_string());
        }
        if matches!(format.as_str(), "png" | "jpg") {
            cmd.args(["-frames:v", "1"]);
        }
        cmd.arg(&out);
        proto::run(&mut cmd, "ffmpeg")?;
        output(
            "artifacts",
            &out,
            proto::media_type_for(&out.to_string_lossy()),
            json!({"source_blake3": input.blake3, "filename": out.file_name().map(|f| f.to_string_lossy().into_owned())}),
        );
        progress(n as u64 + 1, total);
    }
    Ok(())
}

/// `pandoc@1`: `{"to", "from", "template", "standalone"}`; `template` is a workspace file.
pub fn pandoc(shim: &Shim) -> Result<(), ShimError> {
    shim.note_unknown(&["to", "from", "template", "standalone"]);
    let exe = find(&["pandoc"])?;
    let to = shim.str("to")?.unwrap_or("html").to_owned();
    let ext = match to.as_str() {
        "markdown" | "gfm" | "commonmark" | "markdown_strict" => "md",
        "plain" => "txt",
        "latex" => "tex",
        "html" | "html5" => "html",
        other => other,
    };
    let template = match shim.str("template")? {
        Some(t) => {
            let ws = shim.workspace().ok_or_else(|| {
                ShimError::config("template names a workspace file, and no workspace is configured")
            })?;
            let p = ws.join(safe_rel(t)?);
            if !p.is_file() {
                return Err(ShimError::input(
                    "the template does not exist in the workspace",
                ));
            }
            Some(p)
        }
        None => None,
    };
    let dir = shim.dir("artifacts")?;
    for (n, input) in shim.inputs.iter().enumerate() {
        let out = dir.join(format!("{:04}-{}.{ext}", n + 1, sanitize(&input.stem())));
        let mut cmd = Command::new(&exe);
        if let Some(from) = shim.str("from")? {
            cmd.arg("-f").arg(from);
        }
        cmd.arg("-t").arg(&to);
        if shim.bool("standalone")?.unwrap_or(template.is_some()) {
            cmd.arg("-s");
        }
        if let Some(t) = &template {
            cmd.arg("--template").arg(t);
        }
        cmd.arg("--sandbox").arg("-o").arg(&out).arg(&input.path);
        proto::run(&mut cmd, "pandoc")?;
        output(
            "artifacts",
            &out,
            proto::media_type_for(&out.to_string_lossy()),
            json!({"source_blake3": input.blake3, "filename": out.file_name().map(|f| f.to_string_lossy().into_owned())}),
        );
    }
    Ok(())
}

fn input_urls(shim: &Shim) -> Result<Vec<String>, ShimError> {
    let mut urls = shim.strings("urls")?;
    for input in &shim.inputs {
        let f =
            std::fs::File::open(&input.path).map_err(|e| ShimError::io("reading an input", &e))?;
        for line in std::io::BufReader::new(f).lines() {
            let line = line.map_err(|e| ShimError::io("reading an input", &e))?;
            let line = line.trim();
            let url = if line.starts_with('{') {
                serde_json::from_str::<Value>(line)
                    .ok()
                    .and_then(|v| v.get("url").and_then(Value::as_str).map(str::to_owned))
            } else if line.starts_with("http://") || line.starts_with("https://") {
                Some(line.to_owned())
            } else {
                None
            };
            urls.extend(url);
        }
    }
    if urls
        .iter()
        .any(|u| !(u.starts_with("https://") || u.starts_with("http://")))
    {
        return Err(ShimError::config("every URL must be http or https"));
    }
    if urls.is_empty() {
        return Err(ShimError::config("no URLs: give urls or an input of URLs"));
    }
    Ok(urls)
}

/// `yt_dlp@1`: `{"urls", "format", "audio_only", "max_filesize_bytes"}`. Never passes cookies or browser
/// profiles (constraint 2) and ignores every yt-dlp configuration file.
pub fn yt_dlp(shim: &Shim) -> Result<(), ShimError> {
    let exe = find(&["yt-dlp", "yt_dlp"])?;
    let urls = input_urls(shim)?;
    let out = shim.dir("artifacts")?;
    let mut cmd = Command::new(exe);
    cmd.args([
        "--ignore-config",
        "--no-cache-dir",
        "--no-cookies",
        "--no-cookies-from-browser",
        "--no-progress",
        "--restrict-filenames",
        "--no-mtime",
    ])
    .arg("-o")
    .arg(out.join("%(extractor)s-%(id)s.%(ext)s"));
    if let Some(f) = shim.str("format")? {
        cmd.arg("-f").arg(f);
    }
    if shim.bool("audio_only")? == Some(true) {
        cmd.arg("-x");
    }
    if let Some(m) = shim.u64("max_filesize_bytes")? {
        cmd.arg("--max-filesize").arg(m.to_string());
    }
    cmd.arg("--").args(&urls).current_dir(&shim.scratch);
    proto::run(&mut cmd, "yt-dlp")?;
    emit_dir(&out, "artifacts");
    Ok(())
}

/// `rclone@1`: `{"source", "include", "options", "max_transfer_bytes"}`. `options` become `RCLONE_<NAME>`
/// environment variables (values may be secret references); no rclone configuration file is read.
pub fn rclone(shim: &Shim) -> Result<(), ShimError> {
    let exe = find(&["rclone"])?;
    let source = shim
        .str("source")?
        .ok_or_else(|| ShimError::config("rclone needs source"))?
        .to_owned();
    if source.starts_with('-') {
        return Err(ShimError::config("rclone: source must not start with -"));
    }
    let out = shim.dir("artifacts")?;
    let conf = shim.dir(".ogun")?.join("rclone.conf");
    std::fs::write(&conf, "")
        .map_err(|e| ShimError::io("writing an empty rclone configuration", &e))?;
    let mut cmd = Command::new(exe);
    cmd.arg("copy")
        .arg(&source)
        .arg(&out)
        .arg("--config")
        .arg(&conf)
        .arg("--stats")
        .arg("0");
    for inc in shim.strings("include")? {
        cmd.arg("--include").arg(inc);
    }
    if let Some(m) = shim.u64("max_transfer_bytes")? {
        cmd.arg("--max-transfer")
            .arg(format!("{m}B"))
            .arg("--cutoff-mode")
            .arg("hard");
    }
    if let Some(Value::Object(opts)) = shim.config().get("options") {
        for (k, v) in opts {
            if !k
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            {
                return Err(ShimError::config("rclone option names must be [a-z0-9_]"));
            }
            let value = shim.text_of(v).ok_or_else(|| {
                ShimError::config("an rclone option must be a string or a secret_ref")
            })?;
            cmd.env(format!("RCLONE_{}", k.to_ascii_uppercase()), value.as_str());
        }
    }
    proto::run(cmd.current_dir(&shim.scratch), "rclone")?;
    emit_dir(&out, "artifacts");
    Ok(())
}

/// `git@1`: `{"url", "ref", "depth", "token"}`. A shallow fetch of one ref over https, delivered as the
/// `git archive` tar of that commit (metadata `commit`). A token goes in an HTTP header through git's
/// environment configuration, never in the URL.
pub fn git(shim: &Shim) -> Result<(), ShimError> {
    let exe = find(&["git"])?;
    let url = shim
        .str("url")?
        .ok_or_else(|| ShimError::config("git needs url"))?
        .to_owned();
    if !url.starts_with("https://") || url.contains('@') {
        return Err(ShimError::config(
            "git: url must be https without credentials",
        ));
    }
    let reference = shim.str("ref")?.unwrap_or("HEAD").to_owned();
    if reference.starts_with('-') {
        return Err(ShimError::config("git: ref must not start with -"));
    }
    let depth = shim.u64("depth")?.unwrap_or(1);
    let repo = shim.dir(".ogun/repo")?;
    let git = |args: &[&str]| {
        let mut c = Command::new(&exe);
        c.arg("-C")
            .arg(&repo)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_ASKPASS", "/bin/false");
        c
    };
    proto::run(&mut git(&["init", "-q"]), "git")?;
    let mut fetch = git(&["fetch", "-q", "--no-tags", "--depth"]);
    fetch
        .arg(depth.to_string())
        .arg("--")
        .arg(&url)
        .arg(&reference);
    if let Some(token) = shim.config().get("token").and_then(|t| shim.text_of(t)) {
        let basic = proto::base64(format!("x-access-token:{}", token.as_str()).as_bytes());
        fetch
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.extraHeader")
            .env(
                "GIT_CONFIG_VALUE_0",
                format!("Authorization: Basic {}", basic.as_str()),
            );
    }
    proto::run(&mut fetch, "git")?;
    let commit = String::from_utf8_lossy(&proto::run_capture(
        &mut git(&["rev-parse", "FETCH_HEAD"]),
        "git",
    )?)
    .trim()
    .to_owned();
    let out = shim.dir("artifacts")?;
    let tar = out.join("repo.tar");
    let mut archive = git(&["archive", "--format=tar", "-o"]);
    archive.arg(&tar).arg("FETCH_HEAD");
    proto::run(&mut archive, "git")?;
    output(
        "artifacts",
        &tar,
        "application/x-tar",
        json!({"commit": commit, "ref": reference, "filename": "repo.tar"}),
    );
    Ok(())
}

/// `hf@1`: `{"repo_id", "repo_type", "revision", "files", "token"}`. `hf download` (or
/// `huggingface-cli download`) into `artifacts/`; telemetry off.
pub fn hf(shim: &Shim) -> Result<(), ShimError> {
    let exe = find(&["hf", "huggingface-cli"])?;
    let repo = shim
        .str("repo_id")?
        .ok_or_else(|| ShimError::config("hf needs repo_id"))?
        .to_owned();
    if repo.starts_with('-') {
        return Err(ShimError::config("hf: repo_id must not start with -"));
    }
    let out = shim.dir("artifacts")?;
    let mut cmd = Command::new(exe);
    cmd.arg("download").arg(&repo);
    for f in shim.strings("files")? {
        if f.starts_with('-') {
            return Err(ShimError::config("hf: a file name must not start with -"));
        }
        cmd.arg(f);
    }
    if let Some(t) = shim.str("repo_type")? {
        cmd.arg("--repo-type").arg(t);
    }
    if let Some(r) = shim.str("revision")? {
        cmd.arg("--revision").arg(r);
    }
    cmd.arg("--local-dir")
        .arg(&out)
        .env("HF_HUB_DISABLE_TELEMETRY", "1")
        .env("HF_HOME", shim.scratch.join(".ogun/hf"))
        .env("HF_HUB_DISABLE_PROGRESS_BARS", "1");
    if let Some(token) = shim.config().get("token").and_then(|t| shim.text_of(t)) {
        cmd.env("HF_TOKEN", token.as_str());
    }
    proto::run(cmd.current_dir(&shim.scratch), "hf")?;
    emit_dir(&out, "artifacts");
    Ok(())
}
