//! Printing: list printers, print files silently, print the current page of a webview.
//!
//! Windows: PowerShell (hidden) for printer listing / file printing, WebView2 `Print`
//! (ICoreWebView2_16) for silent page printing.
//! Linux/macOS: CUPS command line tools (`lpstat`, `lp`) and the webview print dialog.

use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Quote a string as a PowerShell single-quoted literal.
/// Single quotes (including the typographic variants PowerShell also accepts
/// as quote characters) are doubled, which is the only escaping such literals need.
pub fn ps_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// Parse the output of
/// `Get-CimInstance Win32_Printer | Select-Object Name,Default | ConvertTo-Json -Compress`
/// (a single object, an array of objects, or nothing at all).
pub fn parse_windows_printers(json: &str) -> Result<Vec<Value>, String> {
    let s = json.trim_start_matches('\u{feff}').trim();
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let v: Value = serde_json::from_str(s).map_err(|e| format!("Could not parse the printer list: {e}"))?;
    let items = match v {
        Value::Array(a) => a,
        Value::Object(_) => vec![v],
        Value::Null => Vec::new(),
        _ => return Err("Unexpected printer list format".into()),
    };
    Ok(items
        .iter()
        .filter_map(|p| {
            let name = p.get("Name")?.as_str()?.to_string();
            let default = p.get("Default").and_then(Value::as_bool).unwrap_or(false);
            Some(json!({ "name": name, "default": default }))
        })
        .collect())
}

/// Parse `lpstat -e` (one destination per line) and `lpstat -d`
/// ("system default destination: NAME" or "no system default destination").
pub fn parse_lpstat(names: &str, default: &str) -> Vec<Value> {
    let def = default
        .lines()
        .find_map(|l| {
            let l = l.trim();
            if l.starts_with("no ") {
                return None;
            }
            l.rsplit_once(':').map(|(_, n)| n.trim().to_string())
        })
        .filter(|n| !n.is_empty());
    names
        .lines()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| json!({ "name": n, "default": def.as_deref() == Some(n) }))
        .collect()
}

#[cfg(windows)]
fn powershell(script: &str) -> Result<std::process::Output, String> {
    use std::os::windows::process::CommandExt;
    Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("Could not start PowerShell: {e}"))
}

/// List printers: `[{ "name": String, "default": bool }]`.
pub fn list_printers() -> Result<Vec<Value>, String> {
    #[cfg(windows)]
    {
        let out = powershell(
            "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; \
             Get-CimInstance Win32_Printer | Select-Object Name,Default | ConvertTo-Json -Compress",
        )?;
        if !out.status.success() {
            return Err(format!(
                "Could not list printers: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        parse_windows_printers(&String::from_utf8_lossy(&out.stdout))
    }
    #[cfg(not(windows))]
    {
        let run = |arg: &str| -> Option<String> {
            Command::new("lpstat")
                .arg(arg)
                .env("LC_ALL", "C")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        };
        let Some(names) = run("-e") else { return Ok(Vec::new()) };
        let default = run("-d").unwrap_or_default();
        Ok(parse_lpstat(&names, &default))
    }
}

/// Print a file (PDF, image, txt...) to a printer without dialogs.
/// `printer = None` uses the default printer.
pub fn print_file(path: &Path, printer: Option<&str>) -> Result<(), String> {
    if !path.is_file() {
        return Err(format!("File not found: {}", path.display()));
    }
    let printer = printer.map(str::trim).filter(|p| !p.is_empty());
    #[cfg(windows)]
    {
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let mut file = abs.to_string_lossy().into_owned();
        if let Some(rest) = file.strip_prefix(r"\\?\UNC\") {
            file = format!(r"\\{rest}");
        } else if let Some(rest) = file.strip_prefix(r"\\?\") {
            file = rest.to_string();
        }
        let script = match printer {
            Some(p) => format!(
                "$ErrorActionPreference = 'Stop'; Start-Process -FilePath {} -Verb PrintTo -ArgumentList {} -WindowStyle Hidden",
                ps_quote(&file),
                ps_quote(p)
            ),
            None => format!(
                "$ErrorActionPreference = 'Stop'; Start-Process -FilePath {} -Verb Print -WindowStyle Hidden",
                ps_quote(&file)
            ),
        };
        let out = powershell(&script)?;
        if out.status.success() {
            Ok(())
        } else {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if err.is_empty() {
                format!("Printing failed for {}", path.display())
            } else {
                format!("Printing failed for {}: {err}", path.display())
            })
        }
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("lp");
        if let Some(p) = printer {
            cmd.arg("-d").arg(p);
        }
        // Absolute path so a file name starting with "-" is never taken as an option.
        cmd.arg(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()));
        let out = cmd.output().map_err(|e| format!("Could not run 'lp' (is CUPS installed?): {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!("Printing failed: {}", String::from_utf8_lossy(&out.stderr).trim()))
        }
    }
}

/// Print the current page of a webview.
/// Windows + `silent`: prints directly through WebView2 (no dialog).
/// Otherwise opens the system print dialog. `done` is called exactly once,
/// possibly later on the UI thread.
pub fn print_page(
    webview: &wry::WebView,
    printer: Option<&str>,
    silent: bool,
    done: Box<dyn FnOnce(Result<(), String>)>,
) {
    #[cfg(windows)]
    if silent {
        return win::print_silent(webview, printer.map(str::trim).filter(|p| !p.is_empty()), done);
    }
    let _ = (printer, silent);
    done(webview.print().map_err(|e| format!("Could not open the print dialog: {e}")));
}

#[cfg(windows)]
mod win {
    use std::cell::RefCell;
    use std::rc::Rc;
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::PrintCompletedHandler;
    use windows::core::{Interface, HSTRING};
    use wry::WebViewExtWindows;

    type Done = Box<dyn FnOnce(Result<(), String>)>;

    pub fn print_silent(webview: &wry::WebView, printer: Option<&str>, done: Done) {
        // Shared so that `done` runs exactly once, whether Print fails synchronously
        // or the completion handler fires later.
        let slot: Rc<RefCell<Option<Done>>> = Rc::new(RefCell::new(Some(done)));
        let finish = {
            let slot = slot.clone();
            move |r: Result<(), String>| {
                if let Some(f) = slot.borrow_mut().take() {
                    f(r);
                }
            }
        };
        let printer_label = printer.map(str::to_string);
        let handler = {
            let finish = finish.clone();
            PrintCompletedHandler::create(Box::new(move |hr, status| {
                let r = match hr {
                    Err(e) => Err(format!("Printing failed: {}", e.message())),
                    Ok(()) if status == COREWEBVIEW2_PRINT_STATUS_SUCCEEDED => Ok(()),
                    Ok(()) if status == COREWEBVIEW2_PRINT_STATUS_PRINTER_UNAVAILABLE => Err(match &printer_label {
                        Some(p) => format!("Printer '{p}' is unavailable"),
                        None => "The default printer is unavailable".to_string(),
                    }),
                    Ok(()) => Err("Printing failed (unknown printer error)".to_string()),
                };
                finish(r);
                Ok(())
            }))
        };
        let result = (|| -> Result<(), String> {
            unsafe {
                let core = webview.webview();
                let wv16: ICoreWebView2_16 = core
                    .cast()
                    .map_err(|_| "Silent printing requires a newer WebView2 runtime (ICoreWebView2_16)".to_string())?;
                let wv2: ICoreWebView2_2 = core.cast().map_err(|e| format!("WebView2 error: {}", e.message()))?;
                let env6: ICoreWebView2Environment6 = wv2
                    .Environment()
                    .and_then(|e| e.cast())
                    .map_err(|_| "Silent printing requires a newer WebView2 runtime (ICoreWebView2Environment6)".to_string())?;
                let settings = env6
                    .CreatePrintSettings()
                    .map_err(|e| format!("Could not create print settings: {}", e.message()))?;
                settings
                    .SetShouldPrintBackgrounds(true)
                    .and_then(|_| settings.SetShouldPrintHeaderAndFooter(false))
                    .map_err(|e| format!("Could not configure print settings: {}", e.message()))?;
                if let Some(p) = printer {
                    let s2: ICoreWebView2PrintSettings2 = settings
                        .cast()
                        .map_err(|_| "Choosing a printer requires a newer WebView2 runtime".to_string())?;
                    s2.SetPrinterName(&HSTRING::from(p))
                        .map_err(|e| format!("Could not select printer '{p}': {}", e.message()))?;
                }
                wv16
                    .Print(&settings, &handler)
                    .map_err(|e| format!("Printing failed: {}", e.message()))
            }
        })();
        if let Err(e) = result {
            finish(Err(e));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_ps_quote() {
        assert_eq!(ps_quote("abc"), "'abc'");
        assert_eq!(ps_quote("it's"), "'it''s'");
        assert_eq!(ps_quote("a\u{2019}b"), "'a\u{2019}\u{2019}b'");
        assert_eq!(ps_quote(r"C:\x $y `z"), r"'C:\x $y `z'");
        assert_eq!(ps_quote(""), "''");
    }

    #[test]
    fn print_parse_windows_array() {
        let v = parse_windows_printers(r#"[{"Name":"Zebra ZD420","Default":false},{"Name":"HP","Default":true}]"#).unwrap();
        assert_eq!(v, vec![json!({"name":"Zebra ZD420","default":false}), json!({"name":"HP","default":true})]);
    }

    #[test]
    fn print_parse_windows_single_and_empty() {
        let v = parse_windows_printers("\u{feff}{\"Name\":\"PDF\",\"Default\":true}\r\n").unwrap();
        assert_eq!(v, vec![json!({"name":"PDF","default":true})]);
        assert!(parse_windows_printers("  \r\n").unwrap().is_empty());
        assert!(parse_windows_printers("{\"Default\":true}").unwrap().is_empty());
        assert!(parse_windows_printers("not json").is_err());
    }

    #[test]
    fn print_parse_lpstat() {
        let v = parse_lpstat("Office\nLabel_1\n\n", "system default destination: Label_1\n");
        assert_eq!(v, vec![json!({"name":"Office","default":false}), json!({"name":"Label_1","default":true})]);
        let v = parse_lpstat("Office\n", "no system default destination\n");
        assert_eq!(v, vec![json!({"name":"Office","default":false})]);
        assert!(parse_lpstat("", "").is_empty());
    }
}
