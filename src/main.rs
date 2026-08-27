use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process;

use argparse::{ArgumentParser, Store, StoreTrue};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const MAC_SDK_ROOT: &str =
    "/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk";

#[derive(Default)]
pub struct Config {
    pub path: String,
    pub show_version: bool,
    pub ignore_excludes: bool,
    pub yes: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct ExcludePattern {
    pub pattern: PathBuf,
    pub has_wildcard: bool,
    pub has_dir_wildcard: bool,
}

impl ExcludePattern {
    pub fn pattern_str(&self) -> String {
        let mut pattern = self.pattern.to_string_lossy().to_string();
        if self.has_dir_wildcard {
            pattern.push_str("/%");
        } else if self.has_wildcard {
            pattern.push('%');
        }
        pattern
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum OS {
    Mac,
    Linux,
    Windows,
    Unknown,
}

impl OS {
    pub fn to_str(&self) -> &'static str {
        match self {
            OS::Mac => "Mac",
            OS::Linux => "Linux",
            OS::Windows => "Win32",
            OS::Unknown => "Unknown",
        }
    }

    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            OS::Mac
        } else if cfg!(target_os = "linux") {
            OS::Linux
        } else if cfg!(target_os = "windows") {
            OS::Windows
        } else {
            OS::Unknown
        }
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct CompileCommand {
    pub directory: String,
    pub file: String,
    pub arguments: Vec<String>,
}

fn main() -> io::Result<()> {
    let mut config = Config {
        path: ".".to_string(),
        ..Default::default()
    };

    {
        let desc = format!(
            "openFrameworks Zed Project Generator (for static analysis with clangd) v{}",
            VERSION
        );
        let mut parser = ArgumentParser::new();
        parser.set_description(&desc);
        parser
            .refer(&mut config.path)
            .add_argument("path", Store, "Project path (defaults to current dir)");
        parser
            .refer(&mut config.show_version)
            .add_option(&["-v", "--version"], StoreTrue, "Show version");
        parser
            .refer(&mut config.ignore_excludes)
            .add_option(
                &["-i", "--ignore-excludes"],
                StoreTrue,
                "Ignore addon_config.mk excludes",
            );
        parser.refer(&mut config.yes).add_option(
            &["-y", "--yes"],
            StoreTrue,
            "Automatic yes to prompts; run non-interactively",
        );
        parser.refer(&mut config.dry_run).add_option(
            &["--dry-run"],
            StoreTrue,
            "Show what would be generated without writing to disk",
        );
        parser.parse_args_or_exit();
    }

    env_logger::init();

    if config.show_version {
        println!("of-zed-project-generator-rs v{}", VERSION);
        process::exit(0);
    }

    println!("\n============================================");
    println!("   of-zed-project-generator-rs v{}", VERSION);
    println!("============================================\n");

    let proj_path = if config.path.is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(&config.path)
    };

    if !proj_path.exists() {
        eprintln!("[Error] Project path '{}' does not exist", proj_path.display());
        process::exit(1);
    }

    let proj_path = std::fs::canonicalize(&proj_path)?;
    std::env::set_current_dir(&proj_path)?;

    let os = OS::current();

    println!("------\n");
    if config.ignore_excludes {
        println!("[Info] Ignoring excludes by user (-i / --ignore-excludes) !!!");
    }

    let proj_path_str = normalize_windows_path(proj_path.to_str().unwrap());
    println!("[Info] OS: {}", os.to_str());
    println!("[Info] Project path: '{}'", proj_path_str);

    // Project validation
    if !proj_path.join("src").join("ofApp.h").exists() && !proj_path.join("src").join("main.cpp").exists() {
        println!("[Warning] This directory seems not to be a valid openFrameworks app path (src/ofApp.h not found)!");
        if !config.yes {
            print!("  Are you sure to proceed? (Y/n): ");
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            if input.trim() != "Y" && input.trim() != "y" {
                println!("cancelled.");
                process::exit(1);
            }
        }
    }

    // Check for existing compile_commands.json or .clangd
    let compile_commands_path = proj_path.join("compile_commands.json");
    let clangd_path = proj_path.join(".clangd");
    if compile_commands_path.exists() || clangd_path.exists() {
        println!("[Warning] 'compile_commands.json' or '.clangd' already exists in project root!");
        if !config.yes {
            print!("  Overwrite existing configuration? (Y/n): ");
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            if input.trim() != "Y" && input.trim() != "y" {
                println!("cancelled.");
                process::exit(1);
            }
        }
    }

    // Validate OF root directory
    let of_root = match resolve_path(&proj_path.join("../../..")) {
        Ok(p) => p,
        Err(_) => {
            eprintln!(
                "[Error] '{}' is not an openFrameworks root directory. Expected 'apps' folder in OF root.",
                proj_path.join("../../..").display()
            );
            process::exit(1);
        }
    };

    if !of_root.join("apps").exists() {
        eprintln!(
            "[Error] '{}' is not an openFrameworks root directory (no 'apps' subfolder). Stops.",
            of_root.display()
        );
        process::exit(1);
    }

    // Collect all include directories (fully expanded without wildcards)
    let include_dirs = collect_include_directories(&proj_path, &of_root, os, config.ignore_excludes)?;

    println!("[Info] Collected {} include directories.", include_dirs.len());

    // Discover source files in project (e.g. src/*.cpp, src/**/*.cpp)
    let source_files = collect_source_files(&proj_path)?;
    println!("[Info] Found {} source file(s) in project src/.", source_files.len());

    // Build compile_commands.json content
    let compile_commands = generate_compile_commands(&proj_path_str, &source_files, &include_dirs, os);
    let compile_commands_json_str = serde_json::to_string_pretty(&compile_commands)?;

    // Build .clangd content
    let clangd_yaml_str = generate_clangd_config(&include_dirs, os);

    if config.dry_run {
        println!("\n--- [DRY RUN] compile_commands.json ---");
        println!("{}", compile_commands_json_str);
        println!("\n--- [DRY RUN] .clangd ---");
        println!("{}", clangd_yaml_str);
        println!("\n[Info] Dry run completed. No files were written.");
        return Ok(());
    }

    // Write compile_commands.json
    {
        let mut file = File::create(&compile_commands_path)?;
        file.write_all(compile_commands_json_str.as_bytes())?;
        println!("[Info] Generated '{}'", normalize_windows_path(compile_commands_path.to_str().unwrap()));
    }

    // Write .clangd
    {
        let mut file = File::create(&clangd_path)?;
        file.write_all(clangd_yaml_str.as_bytes())?;
        println!("[Info] Generated '{}'", normalize_windows_path(clangd_path.to_str().unwrap()));
    }

    println!("\n[Success] Project configuration for Zed editor (clangd) completed successfully! :)");
    println!("          Open this directory in Zed to begin editing with static analysis.");

    Ok(())
}

pub fn collect_include_directories(
    proj_path: &Path,
    of_root: &Path,
    os: OS,
    ignore_excludes: bool,
) -> io::Result<Vec<String>> {
    let mut include_paths = HashSet::new();

    // 1. Project directories
    include_paths.insert(normalize_windows_path(proj_path.to_str().unwrap()));
    let src_path = proj_path.join("src");
    if src_path.exists() {
        include_paths.insert(normalize_windows_path(src_path.to_str().unwrap()));
        add_directories_recursively(&src_path, &[], &mut include_paths)?;
    }

    // 2. openFrameworks core library directories
    let of_core = of_root.join("libs").join("openFrameworks");
    if of_core.exists() {
        include_paths.insert(normalize_windows_path(of_core.to_str().unwrap()));
        add_directories_recursively(&of_core, &[], &mut include_paths)?;
    }

    // 3. OF libs (boost, glm, freetype, cairo, etc.)
    let libs_path = of_root.join("libs");
    if libs_path.exists() {
        for entry in fs::read_dir(libs_path)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() && path.file_name().map_or(false, |n| n != "openFrameworks") {
                let include_path = path.join("include");
                if include_path.exists() {
                    include_paths.insert(normalize_windows_path(include_path.to_str().unwrap()));
                    add_directories_recursively(&include_path, &[], &mut include_paths)?;
                }
            }
        }
    }

    // 4. Addons from addons.make
    let addons_path = proj_path.join("addons.make");
    if addons_path.exists() {
        println!("[Info] Reading addons.make");
        let file = File::open(addons_path)?;
        let reader = BufReader::new(file);

        for line in reader.lines() {
            let addon = line?.trim().to_string();
            if addon.is_empty() || addon.starts_with('#') {
                continue;
            }

            let addon_path_raw = if addon.starts_with("addons/") {
                proj_path.join(&addon)
            } else {
                of_root.join("addons").join(&addon)
            };

            let addon_path = match std::fs::canonicalize(&addon_path_raw) {
                Ok(p) => p,
                Err(_) => {
                    warn!("[Warning] Addon path '{}' does not exist. Skipping.", addon_path_raw.display());
                    continue;
                }
            };

            println!("[Info] Checking addon '{}'", normalize_windows_path(addon_path.to_str().unwrap()));

            let excludes = if !ignore_excludes {
                parse_addon_excludes(&addon_path, os)
            } else {
                Vec::new()
            };

            // Add addon src directories
            let addon_src = addon_path.join("src");
            if addon_src.exists() {
                add_directories_recursively(&addon_src, &excludes, &mut include_paths)?;
            }

            // Add addon libs directories
            let addon_libs = addon_path.join("libs");
            if addon_libs.exists() {
                for entry in fs::read_dir(addon_libs)? {
                    let entry = entry?;
                    let lib_path = entry.path();
                    if lib_path.is_dir() {
                        let lib_src = lib_path.join("src");
                        if lib_src.exists() {
                            add_directories_recursively(&lib_src, &excludes, &mut include_paths)?;
                        }
                        let lib_include = lib_path.join("include");
                        if lib_include.exists() {
                            add_directories_recursively(&lib_include, &excludes, &mut include_paths)?;
                        }
                    }
                }
            }
        }
    }

    // 5. Mac SDK paths if on macOS
    if os == OS::Mac {
        let sdk_path = PathBuf::from(MAC_SDK_ROOT);
        if sdk_path.exists() {
            include_paths.insert(format!("{}/usr/include", MAC_SDK_ROOT));
        }
    }

    let mut result: Vec<String> = include_paths.into_iter().collect();
    result.sort();
    Ok(result)
}

pub fn collect_source_files(proj_path: &Path) -> io::Result<Vec<String>> {
    let mut sources = Vec::new();
    let src_dir = proj_path.join("src");

    if src_dir.exists() {
        scan_sources_recursively(proj_path, &src_dir, &mut sources)?;
    }

    if sources.is_empty() {
        // Fallback default source files if none found
        sources.push("src/main.cpp".to_string());
        sources.push("src/ofApp.cpp".to_string());
    }

    sources.sort();
    sources.dedup();
    Ok(sources)
}

fn scan_sources_recursively(
    proj_path: &Path,
    dir: &Path,
    sources: &mut Vec<String>,
) -> io::Result<()> {
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                scan_sources_recursively(proj_path, &path, sources)?;
            } else if let Some(ext) = path.extension() {
                let ext_str = ext.to_string_lossy().to_lowercase();
                if ext_str == "cpp" || ext_str == "cxx" || ext_str == "cc" || ext_str == "c" {
                    if let Ok(rel) = path.strip_prefix(proj_path) {
                        sources.push(normalize_windows_path(rel.to_str().unwrap()));
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn add_directories_recursively(
    dir: &Path,
    excludes: &[ExcludePattern],
    include_paths: &mut HashSet<String>,
) -> io::Result<()> {
    let norm = normalize_windows_path(dir.to_str().unwrap());
    if !is_excluded_dir(dir, excludes) {
        include_paths.insert(norm);
    }

    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                add_directories_recursively(&path, excludes, include_paths)?;
            }
        }
    }
    Ok(())
}

pub fn parse_addon_excludes(addon_path: &Path, os: OS) -> Vec<ExcludePattern> {
    let config_path = addon_path.join("addon_config.mk");
    let mut excludes = Vec::new();

    if !config_path.exists() {
        return excludes;
    }

    let file = match File::open(config_path) {
        Ok(f) => f,
        Err(_) => return excludes,
    };
    let reader = BufReader::new(file);

    let mut current_section: Option<String> = None;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Section header: e.g. linux:, osx:, vs:
        if line.ends_with(':') && !line.contains(' ') && !line.contains('=') {
            current_section = Some(line.trim_end_matches(':').to_string());
            continue;
        }

        // Check if section applies to current OS
        if let Some(ref section) = current_section {
            if section.starts_with("linux") && os != OS::Linux {
                continue;
            }
            if (section.starts_with("osx") || section.starts_with("ios")) && os != OS::Mac {
                continue;
            }
            if (section.starts_with("vs") || section.starts_with("msys2") || section.starts_with("win")) && os != OS::Windows {
                continue;
            }
        }

        if line.starts_with("ADDON_SOURCES_EXCLUDE") || line.starts_with("ADDON_INCLUDES_EXCLUDE") {
            let parts: Vec<&str> = line.split(['=', '+']).collect();
            if parts.len() >= 2 {
                let pattern = parts.last().unwrap().trim();
                if !pattern.is_empty() {
                    let has_dir_wildcard = pattern.ends_with("/%");
                    let has_wildcard = pattern.ends_with('%');
                    let clean_pattern = pattern.trim_end_matches("/%").trim_end_matches('%');
                    excludes.push(ExcludePattern {
                        pattern: addon_path.join(clean_pattern),
                        has_wildcard,
                        has_dir_wildcard,
                    });
                }
            }
        }
    }

    debug!(
        "excludes: {}",
        excludes
            .iter()
            .map(|e| e.pattern_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    excludes
}

pub fn is_excluded_dir(dir_abs: &Path, excludes: &[ExcludePattern]) -> bool {
    let dir_str = normalize_windows_path(dir_abs.to_str().unwrap());

    for exclude in excludes {
        let exclude_str = normalize_windows_path(exclude.pattern.to_str().unwrap());

        if exclude.has_dir_wildcard {
            if dir_str.starts_with(&exclude_str) {
                info!("excluded {} (dir wildcard {}/%)", dir_str, exclude_str);
                return true;
            }
        } else if exclude.has_wildcard {
            if dir_str.starts_with(&exclude_str) {
                info!("excluded {} (wildcard {}%)", dir_str, exclude_str);
                return true;
            }
        } else if dir_str == exclude_str {
            info!("excluded {} (exact match {})", dir_str, exclude_str);
            return true;
        }
    }

    debug!("not excluded {:?}", dir_abs);
    false
}

pub fn generate_compile_commands(
    proj_dir: &str,
    source_files: &[String],
    include_dirs: &[String],
    os: OS,
) -> Vec<CompileCommand> {
    let mut commands = Vec::new();

    let mut common_args = vec!["clang++".to_string(), "-std=c++17".to_string()];

    if os == OS::Windows {
        common_args.push("--target=x86_64-pc-windows-msvc".to_string());
        common_args.push("-DWIN32".to_string());
        common_args.push("-D_WIN32".to_string());
        common_args.push("-DTARGET_WIN32".to_string());
        common_args.push("-D_CRT_SECURE_NO_WARNINGS".to_string());
    } else if os == OS::Mac {
        common_args.push("-DTARGET_OSX".to_string());
        let sdk_path = Path::new(MAC_SDK_ROOT);
        if sdk_path.exists() {
            common_args.push("-isysroot".to_string());
            common_args.push(MAC_SDK_ROOT.to_string());
            common_args.push("-F".to_string());
            common_args.push(format!("{}/System/Library/Frameworks", MAC_SDK_ROOT));
        }
    } else if os == OS::Linux {
        common_args.push("-DTARGET_LINUX".to_string());
    }

    for dir in include_dirs {
        common_args.push(format!("-I{}", dir));
    }

    for src_file in source_files {
        let mut args = common_args.clone();
        args.push("-c".to_string());
        args.push(src_file.clone());

        commands.push(CompileCommand {
            directory: proj_dir.to_string(),
            file: src_file.clone(),
            arguments: args,
        });
    }

    commands
}

pub fn generate_clangd_config(include_dirs: &[String], os: OS) -> String {
    let mut yaml = String::new();
    yaml.push_str("# Generated by of-zed-project-generator-rs for Zed editor\n");
    yaml.push_str("# yaml-language-server: $schema=https://json.schemastore.org/clangd.json\n\n");
    yaml.push_str("CompileFlags:\n");
    yaml.push_str("  CompilationDatabase: .\n");
    yaml.push_str("  Add:\n");
    yaml.push_str("    - -std=c++17\n");

    if os == OS::Windows {
        yaml.push_str("    - --target=x86_64-pc-windows-msvc\n");
        yaml.push_str("    - -DWIN32\n");
        yaml.push_str("    - -D_WIN32\n");
        yaml.push_str("    - -DTARGET_WIN32\n");
        yaml.push_str("    - -D_CRT_SECURE_NO_WARNINGS\n");
    } else if os == OS::Mac {
        yaml.push_str("    - -DTARGET_OSX\n");
        let sdk_path = Path::new(MAC_SDK_ROOT);
        if sdk_path.exists() {
            yaml.push_str(&format!("    - -isysroot\n    - {}\n", MAC_SDK_ROOT));
            yaml.push_str(&format!(
                "    - -F{}/System/Library/Frameworks\n",
                MAC_SDK_ROOT
            ));
        }
    } else if os == OS::Linux {
        yaml.push_str("    - -DTARGET_LINUX\n");
    }

    for dir in include_dirs {
        yaml.push_str(&format!("    - -I{}\n", dir));
    }

    yaml
}

fn resolve_path(path: &Path) -> io::Result<PathBuf> {
    if path.exists() {
        let canonical = std::fs::canonicalize(path)?;
        if cfg!(target_os = "windows") {
            Ok(PathBuf::from(normalize_windows_path(
                canonical.to_str().unwrap(),
            )))
        } else {
            Ok(canonical)
        }
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Path '{}' does not exist", path.display()),
        ))
    }
}

pub fn normalize_windows_path(path_str: &str) -> String {
    let mut result = path_str.to_string();

    if result.starts_with(r"\\?\") {
        result = result[4..].to_string();
    }

    result.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_windows_path() {
        assert_eq!(
            normalize_windows_path(r"C:\Users\foo\bar"),
            "C:/Users/foo/bar"
        );
        assert_eq!(
            normalize_windows_path(r"\\?\C:\Users\foo\bar"),
            "C:/Users/foo/bar"
        );
        assert_eq!(
            normalize_windows_path("/Users/foo/bar"),
            "/Users/foo/bar"
        );
    }

    #[test]
    fn test_os_to_str() {
        assert_eq!(OS::Mac.to_str(), "Mac");
        assert_eq!(OS::Linux.to_str(), "Linux");
        assert_eq!(OS::Windows.to_str(), "Win32");
        assert_eq!(OS::Unknown.to_str(), "Unknown");
    }

    #[test]
    fn test_generate_compile_commands() {
        let proj_dir = "C:/my_project";
        let source_files = vec!["src/main.cpp".to_string(), "src/ofApp.cpp".to_string()];
        let include_dirs = vec!["C:/of/libs/openFrameworks".to_string(), "C:/my_project/src".to_string()];

        let cmds = generate_compile_commands(proj_dir, &source_files, &include_dirs, OS::Linux);
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0].directory, "C:/my_project");
        assert_eq!(cmds[0].file, "src/main.cpp");
        assert_eq!(
            cmds[0].arguments,
            vec![
                "clang++",
                "-std=c++17",
                "-DTARGET_LINUX",
                "-IC:/of/libs/openFrameworks",
                "-IC:/my_project/src",
                "-c",
                "src/main.cpp"
            ]
        );

        let win_cmds = generate_compile_commands(proj_dir, &source_files, &include_dirs, OS::Windows);
        assert!(win_cmds[0].arguments.contains(&"--target=x86_64-pc-windows-msvc".to_string()));
        assert!(win_cmds[0].arguments.contains(&"-DWIN32".to_string()));
        assert!(win_cmds[0].arguments.contains(&"-DTARGET_WIN32".to_string()));
    }

    #[test]
    fn test_generate_clangd_config() {
        let include_dirs = vec!["C:/of/libs/openFrameworks".to_string(), "C:/my_project/src".to_string()];
        let yaml = generate_clangd_config(&include_dirs, OS::Windows);

        assert!(yaml.contains("CompilationDatabase: ."));
        assert!(yaml.contains("- -std=c++17"));
        assert!(yaml.contains("- --target=x86_64-pc-windows-msvc"));
        assert!(yaml.contains("- -DWIN32"));
        assert!(yaml.contains("- -DTARGET_WIN32"));
        assert!(yaml.contains("- -IC:/of/libs/openFrameworks"));
        assert!(yaml.contains("- -IC:/my_project/src"));
    }

    #[test]
    fn test_exclude_pattern_matching() {
        let excludes = vec![
            ExcludePattern {
                pattern: PathBuf::from("C:/addon/libs/excluded_lib"),
                has_wildcard: false,
                has_dir_wildcard: true,
            },
            ExcludePattern {
                pattern: PathBuf::from("C:/addon/src/excluded_file.cpp"),
                has_wildcard: false,
                has_dir_wildcard: false,
            },
        ];

        assert!(is_excluded_dir(
            Path::new(r"C:\addon\libs\excluded_lib\sub"),
            &excludes
        ));
        assert!(is_excluded_dir(
            Path::new(r"C:\addon\src\excluded_file.cpp"),
            &excludes
        ));
        assert!(!is_excluded_dir(
            Path::new(r"C:\addon\libs\included_lib"),
            &excludes
        ));
    }

    #[test]
    fn test_mock_openframeworks_project_generation() {
        let temp_dir = std::env::temp_dir().join(format!("test_of_proj_{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);

        let of_root = temp_dir.join("openFrameworks");
        let proj_dir = of_root.join("apps").join("myApps").join("mockApp");

        // Setup mock openFrameworks directory structure
        fs::create_dir_all(of_root.join("libs").join("openFrameworks").join("app")).unwrap();
        fs::create_dir_all(of_root.join("libs").join("openFrameworks").join("graphics")).unwrap();
        fs::create_dir_all(of_root.join("libs").join("glm").join("include").join("glm")).unwrap();
        fs::create_dir_all(of_root.join("addons").join("ofxGui").join("src")).unwrap();
        fs::create_dir_all(proj_dir.join("src").join("utils")).unwrap();

        // Create dummy files
        fs::write(proj_dir.join("src").join("main.cpp"), "int main() {}").unwrap();
        fs::write(proj_dir.join("src").join("ofApp.cpp"), "#include \"ofApp.h\"").unwrap();
        fs::write(proj_dir.join("src").join("ofApp.h"), "#pragma once").unwrap();
        fs::write(proj_dir.join("src").join("utils").join("helper.cpp"), "// helper").unwrap();
        fs::write(proj_dir.join("addons.make"), "ofxGui\n").unwrap();
        fs::write(
            of_root.join("addons").join("ofxGui").join("addon_config.mk"),
            "meta:\n\tADDON_NAME = ofxGui\n",
        )
        .unwrap();

        let os = OS::Linux;
        let include_dirs = collect_include_directories(&proj_dir, &of_root, os, false).unwrap();
        let source_files = collect_source_files(&proj_dir).unwrap();

        // Verify source files found recursively
        assert!(source_files.contains(&"src/main.cpp".to_string()));
        assert!(source_files.contains(&"src/ofApp.cpp".to_string()));
        assert!(source_files.contains(&"src/utils/helper.cpp".to_string()));
        assert_eq!(source_files.len(), 3);

        // Verify include dirs contain all necessary paths
        let proj_dir_norm = normalize_windows_path(proj_dir.to_str().unwrap());
        let of_root_norm = normalize_windows_path(of_root.to_str().unwrap());

        assert!(include_dirs.iter().any(|d| d.contains(&format!("{}/src/utils", proj_dir_norm))));
        assert!(include_dirs.iter().any(|d| d.contains(&format!("{}/libs/openFrameworks/app", of_root_norm))));
        assert!(include_dirs.iter().any(|d| d.contains(&format!("{}/libs/glm/include", of_root_norm))));
        assert!(include_dirs.iter().any(|d| d.contains(&format!("{}/addons/ofxGui/src", of_root_norm))));

        // Generate compile commands and .clangd
        let commands = generate_compile_commands(&proj_dir_norm, &source_files, &include_dirs, os);
        assert_eq!(commands.len(), 3);
        for cmd in &commands {
            assert_eq!(cmd.directory, proj_dir_norm);
            assert!(cmd.arguments.contains(&"-std=c++17".to_string()));
            assert!(cmd.arguments.contains(&format!("-I{}/src", proj_dir_norm)));
        }

        let clangd_yaml = generate_clangd_config(&include_dirs, os);
        assert!(clangd_yaml.contains("CompilationDatabase: ."));
        assert!(clangd_yaml.contains("- -std=c++17"));
        assert!(clangd_yaml.contains(&format!("- -I{}/src", proj_dir_norm)));

        // Cleanup
        let _ = fs::remove_dir_all(&temp_dir);
    }
}

