//! An implementation of [tldr](https://github.com/tldr-pages/tldr) in Rust.
//
// Copyright (c) 2015-2021 tealdeer developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. All files in the project carrying such notice may not be
// copied, modified, or distributed except according to those terms.

#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![allow(clippy::enum_glob_use)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::similar_names)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::too_many_lines)]

#[cfg(any(
    all(feature = "native-roots", feature = "webpki-roots"),
    not(any(feature = "native-roots", feature = "webpki-roots")),
))]
compile_error!(
    "exactly one of feature \"native-roots\" and feature \"webpki-roots\" must be enabled"
);

use std::{env, process};

use app_dirs::AppInfo;
use atty::Stream;
use clap::Parser;

mod cache;
mod cli;
mod config;
pub mod extensions;
mod formatter;
mod line_iterator;
mod output;
mod types;
mod utils;

use crate::{
    cache::{Cache, CacheFreshness, PageLookupResult, TLDR_PAGES_DIR},
    cli::Args,
    config::{get_config_dir, get_config_path, make_default_config, Config},
    extensions::Dedup,
    output::print_page,
    types::{ColorOptions, PlatformType},
    utils::{print_error, print_warning},
};

const NAME: &str = "tealdeer";
const APP_INFO: AppInfo = AppInfo {
    name: NAME,
    author: NAME,
};
const ARCHIVE_URL: &str = "https://tldr.sh/assets/tldr.zip";

/// The cache should be updated if it was explicitly requested,
/// or if an automatic update is due and allowed.
fn should_update_cache(args: &Args, config: &Config) -> bool {
    args.update
        || (!args.no_auto_update
            && config.updates.auto_update
            && Cache::last_update().map_or(true, |ago| ago >= config.updates.auto_update_interval))
}

#[derive(PartialEq)]
enum CheckCacheResult {
    CacheFound,
    CacheMissing,
}

/// Check the cache for freshness. If it's stale or missing, show a warning.
fn check_cache(args: &Args, enable_styles: bool) -> CheckCacheResult {
    match Cache::freshness() {
        CacheFreshness::Fresh => CheckCacheResult::CacheFound,
        CacheFreshness::Stale(_) if args.quiet => CheckCacheResult::CacheFound,
        CacheFreshness::Stale(age) => {
            print_warning(
                enable_styles,
                &format!(
                    "The cache hasn't been updated for {} days.\n\
                     You should probably run `tldr --update` soon.",
                    age.as_secs() / 24 / 3600
                ),
            );
            CheckCacheResult::CacheFound
        }
        CacheFreshness::Missing => {
            print_error(
                enable_styles,
                &anyhow::anyhow!(
                    "Page cache not found. Please run `tldr --update` to download the cache."
                ),
            );
            println!("\nNote: You can optionally enable automatic cache updates by adding the");
            println!("following config to your config file:\n");
            println!("  [updates]");
            println!("  auto_update = true\n");
            println!("The path to your config file can be looked up with `tldr --show-paths`.");
            println!("To create an initial config file, use `tldr --seed-config`.\n");
            println!("You can find more tips and tricks in our docs:\n");
            println!("  https://dbrgn.github.io/tealdeer/config_updates.html");
            CheckCacheResult::CacheMissing
        }
    }
}

/// Clear the cache
fn clear_cache(quietly: bool, enable_styles: bool) {
    Cache::clear().unwrap_or_else(|e| {
        print_error(enable_styles, &e.context("Could not clear cache"));
        process::exit(1);
    });
    if !quietly {
        eprintln!("Successfully deleted cache.");
    }
}

/// Update the cache
fn update_cache(cache: &Cache, quietly: bool, enable_styles: bool) {
    cache.update().unwrap_or_else(|e| {
        print_error(enable_styles, &e.context("Could not update cache"));
        process::exit(1);
    });
    if !quietly {
        eprintln!("Successfully updated cache.");
    }
}

/// Show the config path (DEPRECATED)
fn show_config_path(enable_styles: bool) {
    match get_config_path() {
        Ok((config_file_path, _)) => {
            println!("Config path is: {}", config_file_path.to_str().unwrap());
        }
        Err(e) => {
            print_error(enable_styles, &e.context("Could not look up config path"));
            process::exit(1);
        }
    }
}

/// Show file paths
fn show_paths(config: &Config) {
    let config_dir = get_config_dir().map_or_else(
        |e| format!("[Error: {}]", e),
        |(mut path, source)| {
            path.push(""); // Trailing path separator
            match path.to_str() {
                Some(path) => format!("{} ({})", path, source),
                None => "[Invalid]".to_string(),
            }
        },
    );
    let config_path = get_config_path().map_or_else(
        |e| format!("[Error: {}]", e),
        |(path, _)| path.to_str().unwrap_or("[Invalid]").to_string(),
    );
    let cache_dir = Cache::get_cache_dir().map_or_else(
        |e| format!("[Error: {}]", e),
        |(mut path, source)| {
            path.push(""); // Trailing path separator
            match path.to_str() {
                Some(path) => format!("{} ({})", path, source),
                None => "[Invalid]".to_string(),
            }
        },
    );
    let pages_dir = Cache::get_cache_dir().map_or_else(
        |e| format!("[Error: {}]", e),
        |(mut path, _)| {
            path.push(TLDR_PAGES_DIR);
            path.push(""); // Trailing path separator
            path.into_os_string()
                .into_string()
                .unwrap_or_else(|_| "[Invalid]".to_string())
        },
    );
    let custom_pages_dir = config.directories.custom_pages_dir.as_deref().map_or_else(
        || "[None]".to_string(),
        |path| {
            path.to_str()
                .map_or_else(|| "[Invalid]".to_string(), ToString::to_string)
        },
    );
    println!("Config dir:       {}", config_dir);
    println!("Config path:      {}", config_path);
    println!("Cache dir:        {}", cache_dir);
    println!("Pages dir:        {}", pages_dir);
    println!("Custom pages dir: {}", custom_pages_dir);
}

/// Create seed config file and exit
fn create_config_and_exit(enable_styles: bool) {
    match make_default_config() {
        Ok(config_file_path) => {
            eprintln!(
                "Successfully created seed config file here: {}",
                config_file_path.to_str().unwrap()
            );
            process::exit(0);
        }
        Err(e) => {
            print_error(enable_styles, &e.context("Could not create seed config"));
            process::exit(1);
        }
    }
}

#[cfg(feature = "logging")]
fn init_log() {
    env_logger::init();
}

#[cfg(not(feature = "logging"))]
fn init_log() {}

fn get_languages(env_lang: Option<&str>, env_language: Option<&str>) -> Vec<String> {
    // Language list according to
    // https://github.com/tldr-pages/tldr/blob/main/CLIENT-SPECIFICATION.md#language

    if env_lang.is_none() {
        return vec!["en".to_string()];
    }
    let env_lang = env_lang.unwrap();

    // Create an iterator that contains $LANGUAGE (':' separated list) followed by $LANG (single language)
    let locales = env_language.unwrap_or("").split(':').chain([env_lang]);

    let mut lang_list = Vec::new();
    for locale in locales {
        // Language plus country code (e.g. `en_US`)
        if locale.len() >= 5 && locale.chars().nth(2) == Some('_') {
            lang_list.push(&locale[..5]);
        }
        // Language code only (e.g. `en`)
        if locale.len() >= 2 && locale != "POSIX" {
            lang_list.push(&locale[..2]);
        }
    }

    lang_list.push("en");
    lang_list.clear_duplicates();
    lang_list.into_iter().map(str::to_string).collect()
}

fn get_languages_from_env() -> Vec<String> {
    get_languages(
        std::env::var("LANG").ok().as_deref(),
        std::env::var("LANGUAGE").ok().as_deref(),
    )
}

fn main() {
    // Initialize logger
    init_log();

    // Parse arguments
    let mut args = Args::parse();

    // Determine the usage of styles
    #[cfg(target_os = "windows")]
    let ansi_support = ansi_term::enable_ansi_support().is_ok();
    #[cfg(not(target_os = "windows"))]
    let ansi_support = true;
    let enable_styles = match args.color.unwrap_or_default() {
        // Attempt to use styling if instructed
        ColorOptions::Always => true,
        // Enable styling if:
        // * There is `ansi_support`
        // * NO_COLOR env var isn't set: https://no-color.org/
        // * The output stream is stdout (not being piped)
        ColorOptions::Auto => {
            ansi_support && env::var_os("NO_COLOR").is_none() && atty::is(Stream::Stdout)
        }
        // Disable styling
        ColorOptions::Never => false,
    };

    // Handle renamed arguments
    if args.markdown {
        args.raw = true;
        print_warning(
            enable_styles,
            "The -m / --markdown flag is deprecated, use -r / --raw instead",
        );
    }
    if args.os.is_some() {
        print_warning(
            enable_styles,
            "The -o / --os flag is deprecated, use -p / --platform instead",
        );
    }
    args.platform = args.platform.or(args.os);

    // Show config file and path, pass through
    if args.config_path {
        print_warning(
            enable_styles,
            "The --config-path flag is deprecated, use --show-paths instead",
        );
        show_config_path(enable_styles);
    }

    // Look up config file, if none is found fall back to default config.
    let config = match Config::load(enable_styles) {
        Ok(config) => config,
        Err(e) => {
            print_error(enable_styles, &e.context("Could not load config"));
            process::exit(1);
        }
    };

    // Show various paths
    if args.show_paths {
        show_paths(&config);
    }

    // Create a basic config and exit
    if args.seed_config {
        create_config_and_exit(enable_styles);
    }

    // Specify target OS
    let platform: PlatformType = args.platform.unwrap_or_else(PlatformType::current);

    // If a local file was passed in, render it and exit
    if let Some(file) = args.render {
        let path = PageLookupResult::with_page(file);
        if let Err(ref e) = print_page(&path, args.raw, enable_styles, args.pager, &config) {
            print_error(enable_styles, e);
            process::exit(1);
        } else {
            process::exit(0);
        };
    }

    // Initialize cache
    let cache = Cache::new(ARCHIVE_URL, platform);

    // Clear cache, pass through
    if args.clear_cache {
        clear_cache(args.quiet, enable_styles);
    }

    // Cache update, pass through
    let cache_updated = if should_update_cache(&args, &config) {
        update_cache(&cache, args.quiet, enable_styles);
        true
    } else {
        false
    };

    // Check cache presence and freshness
    if !cache_updated
        && (args.list || !args.command.is_empty())
        && check_cache(&args, enable_styles) == CheckCacheResult::CacheMissing
    {
        process::exit(1);
    }

    // List cached commands and exit
    if args.list {
        // Get list of pages
        let pages = cache.list_pages().unwrap_or_else(|e| {
            print_error(enable_styles, &e.context("Could not get list of pages"));
            process::exit(1);
        });

        // Print pages
        println!("{}", pages.join("\n"));
        process::exit(0);
    }

    // Show command from cache
    if !args.command.is_empty() {
        // Note: According to the TLDR client spec, page names must be transparently
        // lowercased before lookup:
        // https://github.com/tldr-pages/tldr/blob/main/CLIENT-SPECIFICATION.md#page-names
        let command = args.command.join("-").to_lowercase();

        // Collect languages
        let languages = args
            .language
            .map_or_else(get_languages_from_env, |lang| vec![lang]);

        // Search for command in cache
        if let Some(lookup_result) = cache.find_page(
            &command,
            &languages,
            config.directories.custom_pages_dir.as_deref(),
        ) {
            if let Err(ref e) =
                print_page(&lookup_result, args.raw, enable_styles, args.pager, &config)
            {
                print_error(enable_styles, e);
                process::exit(1);
            }
            process::exit(0);
        } else {
            if !args.quiet {
                print_warning(
                    enable_styles,
                    &format!(
                        "Page `{}` not found in cache.\n\
                         Try updating with `tldr --update`, or submit a pull request to:\n\
                         https://github.com/tldr-pages/tldr",
                        &command
                    ),
                );
            }
            process::exit(1);
        }
    }
}

#[cfg(test)]
mod test {
    use crate::get_languages;

    mod language {
        use super::*;

        #[test]
        fn missing_lang_env() {
            let lang_list = get_languages(None, Some("de:fr"));
            assert_eq!(lang_list, ["en"]);
            let lang_list = get_languages(None, None);
            assert_eq!(lang_list, ["en"]);
        }

        #[test]
        fn missing_language_env() {
            let lang_list = get_languages(Some("de"), None);
            assert_eq!(lang_list, ["de", "en"]);
        }

        #[test]
        fn preference_order() {
            let lang_list = get_languages(Some("de"), Some("fr:cn"));
            assert_eq!(lang_list, ["fr", "cn", "de", "en"]);
        }

        #[test]
        fn country_code_expansion() {
            let lang_list = get_languages(Some("pt_BR"), None);
            assert_eq!(lang_list, ["pt_BR", "pt", "en"]);
        }

        #[test]
        fn ignore_posix_and_c() {
            let lang_list = get_languages(Some("POSIX"), None);
            assert_eq!(lang_list, ["en"]);
            let lang_list = get_languages(Some("C"), None);
            assert_eq!(lang_list, ["en"]);
        }

        #[test]
        fn no_duplicates() {
            let lang_list = get_languages(Some("de"), Some("fr:de:cn:de"));
            assert_eq!(lang_list, ["fr", "de", "cn", "en"]);
        }
    }
}
