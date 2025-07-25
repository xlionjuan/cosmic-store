// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use cosmic::{
    action,
    app::{context_drawer, Core, CosmicFlags, Settings, Task},
    cosmic_config::{self, CosmicConfigEntry},
    cosmic_theme, executor,
    iced::{
        core::SmolStr,
        event::{self, Event},
        futures::{self, SinkExt},
        keyboard::{Event as KeyEvent, Key, Modifiers},
        stream,
        widget::scrollable,
        window::{self, Event as WindowEvent},
        Alignment, Length, Limits, Size, Subscription,
    },
    theme, widget, Application, ApplicationExt, Element,
};
use localize::LANGUAGE_SORTER;
use rayon::prelude::*;
use std::{
    any::TypeId,
    cmp,
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    env,
    future::pending,
    path::Path,
    process,
    sync::{Arc, Mutex},
    time::Instant,
};

use app_id::AppId;
mod app_id;

use app_info::{AppIcon, AppInfo, AppProvide, AppUrl};
mod app_info;

use appstream_cache::AppstreamCache;
mod appstream_cache;

use backend::{Backends, Package};
mod backend;

use config::{AppTheme, Config, CONFIG_VERSION};
mod config;

use editors_choice::EDITORS_CHOICE;
mod editors_choice;

use icon_cache::{icon_cache_handle, icon_cache_icon};
mod icon_cache;

use key_bind::{key_binds, KeyBind};
mod key_bind;

mod localize;

#[cfg(feature = "logind")]
mod logind;

use operation::{Operation, OperationKind};
mod operation;

use priority::priority;
mod priority;

mod stats;

const ICON_SIZE_SEARCH: u16 = 48;
const ICON_SIZE_PACKAGE: u16 = 64;
const ICON_SIZE_DETAILS: u16 = 128;
const MAX_GRID_WIDTH: f32 = 1600.0;
const MAX_RESULTS: usize = 100;

/// Runs application with these settings
#[rustfmt::skip]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    localize::localize();

    //TODO: more advanced argument parsing
    let subcommand_opt = env::args().nth(1);

    let (config_handler, config) = match cosmic_config::Config::new(App::APP_ID, CONFIG_VERSION) {
        Ok(config_handler) => {
            let config = match Config::get_entry(&config_handler) {
                Ok(ok) => ok,
                Err((errs, config)) => {
                    log::info!("errors loading config: {:?}", errs);
                    config
                }
            };
            (Some(config_handler), config)
        }
        Err(err) => {
            log::error!("failed to create config handler: {}", err);
            (None, Config::default())
        }
    };

    let mut settings = Settings::default();
    settings = settings.theme(config.app_theme.theme());
    settings = settings.size_limits(Limits::NONE.min_width(360.0).min_height(180.0));
    settings = settings.exit_on_close(false);

    let flags = Flags {
        subcommand_opt,
        config_handler,
        config,
    };

    #[cfg(feature = "single-instance")]
    cosmic::app::run_single_instance::<App>(settings, flags)?;

    #[cfg(not(feature = "single-instance"))]
    cosmic::app::run::<App>(settings, flags)?;

    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    SearchActivate,
}

impl Action {
    pub fn message(&self) -> Message {
        match self {
            Self::SearchActivate => Message::SearchActivate,
        }
    }
}

pub struct AppEntry {
    backend_name: &'static str,
    info: Arc<AppInfo>,
    installed: bool,
}

pub type Apps = HashMap<AppId, Vec<AppEntry>>;

#[derive(Clone, Debug)]
pub struct Flags {
    subcommand_opt: Option<String>,
    config_handler: Option<cosmic_config::Config>,
    config: Config,
}

//TODO
impl CosmicFlags for Flags {
    type SubCommand = String;
    type Args = Vec<String>;

    fn action(&self) -> Option<&String> {
        self.subcommand_opt.as_ref()
    }
}

/// Messages that are used specifically by our [`App`].
#[derive(Clone, Debug)]
pub enum Message {
    AppTheme(AppTheme),
    Backends(Backends),
    CategoryResults(&'static [Category], Vec<SearchResult>),
    CheckUpdates,
    Config(Config),
    DialogCancel,
    DialogConfirm,
    DialogPage(DialogPage),
    ExplorePage(Option<ExplorePage>),
    ExploreResults(ExplorePage, Vec<SearchResult>),
    Installed(Vec<(&'static str, Package)>),
    InstalledResults(Vec<SearchResult>),
    Key(Modifiers, Key, Option<SmolStr>),
    LaunchUrl(String),
    MaybeExit,
    #[cfg(feature = "notify")]
    Notification(Arc<Mutex<notify_rust::NotificationHandle>>),
    OpenDesktopId(String),
    Operation(OperationKind, &'static str, AppId, Arc<AppInfo>),
    PendingComplete(u64),
    PendingDismiss,
    PendingError(u64, String),
    PendingProgress(u64, f32),
    ScrollView(scrollable::Viewport),
    SearchActivate,
    SearchClear,
    SearchInput(String),
    SearchResults(String, Vec<SearchResult>, bool),
    SearchSubmit(String),
    Select(
        &'static str,
        AppId,
        Option<widget::icon::Handle>,
        Arc<AppInfo>,
    ),
    SelectInstalled(usize),
    SelectUpdates(usize),
    SelectNone,
    SelectCategoryResult(usize),
    SelectExploreResult(ExplorePage, usize),
    SelectSearchResult(usize),
    SelectedScreenshot(usize, String, Vec<u8>),
    SelectedScreenshotShown(usize),
    SelectedSource(usize),
    SystemThemeModeChange(cosmic_theme::ThemeMode),
    ToggleContextPage(ContextPage),
    UpdateAll,
    Updates(Vec<(&'static str, Package)>),
    WindowClose,
    WindowNew,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextPage {
    Operations,
    ReleaseNotes(usize, String),
    Settings,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DialogPage {
    FailedOperation(u64),
    Uninstall(&'static str, AppId, Arc<AppInfo>),
}

// From https://specifications.freedesktop.org/menu-spec/latest/apa.html
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Category {
    AudioVideo,
    Development,
    Education,
    Game,
    Graphics,
    Network,
    Office,
    Science,
    Settings,
    System,
    Utility,
    CosmicApplet,
}

impl Category {
    fn id(&self) -> &'static str {
        match self {
            Self::AudioVideo => "AudioVideo",
            Self::Development => "Development",
            Self::Education => "Education",
            Self::Game => "Game",
            Self::Graphics => "Graphics",
            Self::Network => "Network",
            Self::Office => "Office",
            Self::Science => "Science",
            Self::Settings => "Settings",
            Self::System => "System",
            Self::Utility => "Utility",
            Self::CosmicApplet => "CosmicApplet",
        }
    }
}

#[derive(Clone, Copy, Default, Debug, Eq, PartialEq)]
pub enum NavPage {
    #[default]
    Explore,
    Create,
    Work,
    Develop,
    Learn,
    Game,
    Relax,
    Socialize,
    Utilities,
    Applets,
    Installed,
    Updates,
}

impl NavPage {
    fn all() -> &'static [Self] {
        &[
            Self::Explore,
            Self::Create,
            Self::Work,
            Self::Develop,
            Self::Learn,
            Self::Game,
            Self::Relax,
            Self::Socialize,
            Self::Utilities,
            Self::Applets,
            Self::Installed,
            Self::Updates,
        ]
    }

    fn title(&self) -> String {
        match self {
            Self::Explore => fl!("explore"),
            Self::Create => fl!("create"),
            Self::Work => fl!("work"),
            Self::Develop => fl!("develop"),
            Self::Learn => fl!("learn"),
            Self::Game => fl!("game"),
            Self::Relax => fl!("relax"),
            Self::Socialize => fl!("socialize"),
            Self::Utilities => fl!("utilities"),
            Self::Applets => fl!("applets"),
            Self::Installed => fl!("installed-apps"),
            Self::Updates => fl!("updates"),
        }
    }

    // From https://specifications.freedesktop.org/menu-spec/latest/apa.html
    fn categories(&self) -> Option<&'static [Category]> {
        match self {
            Self::Create => Some(&[Category::AudioVideo, Category::Graphics]),
            Self::Work => Some(&[Category::Development, Category::Office, Category::Science]),
            Self::Develop => Some(&[Category::Development]),
            Self::Learn => Some(&[Category::Education]),
            Self::Game => Some(&[Category::Game]),
            Self::Relax => Some(&[Category::AudioVideo]),
            Self::Socialize => Some(&[Category::Network]),
            Self::Utilities => Some(&[Category::Settings, Category::System, Category::Utility]),
            Self::Applets => Some(&[Category::CosmicApplet]),
            _ => None,
        }
    }

    fn icon(&self) -> widget::icon::Icon {
        match self {
            Self::Explore => icon_cache_icon("store-home-symbolic", 16),
            Self::Create => icon_cache_icon("store-create-symbolic", 16),
            Self::Work => icon_cache_icon("store-work-symbolic", 16),
            Self::Develop => icon_cache_icon("store-develop-symbolic", 16),
            Self::Learn => icon_cache_icon("store-learn-symbolic", 16),
            Self::Game => icon_cache_icon("store-game-symbolic", 16),
            Self::Relax => icon_cache_icon("store-relax-symbolic", 16),
            Self::Socialize => icon_cache_icon("store-socialize-symbolic", 16),
            Self::Utilities => icon_cache_icon("store-utilities-symbolic", 16),
            Self::Applets => icon_cache_icon("store-applets-symbolic", 16),
            Self::Installed => icon_cache_icon("store-installed-symbolic", 16),
            Self::Updates => icon_cache_icon("store-updates-symbolic", 16),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ExplorePage {
    EditorsChoice,
    PopularApps,
    MadeForCosmic,
    NewApps,
    RecentlyUpdated,
    DevelopmentTools,
    ScientificTools,
    ProductivityApps,
    GraphicsAndPhotographyTools,
    SocialNetworkingApps,
    Games,
    MusicAndVideoApps,
    AppsForLearning,
    Utilities,
}

impl ExplorePage {
    fn all() -> &'static [Self] {
        &[
            Self::EditorsChoice,
            Self::PopularApps,
            Self::MadeForCosmic,
            //TODO: Self::NewApps,
            Self::RecentlyUpdated,
            Self::DevelopmentTools,
            Self::ScientificTools,
            Self::ProductivityApps,
            Self::GraphicsAndPhotographyTools,
            Self::SocialNetworkingApps,
            Self::Games,
            Self::MusicAndVideoApps,
            Self::AppsForLearning,
            Self::Utilities,
        ]
    }

    fn title(&self) -> String {
        match self {
            Self::EditorsChoice => fl!("editors-choice"),
            Self::PopularApps => fl!("popular-apps"),
            Self::MadeForCosmic => fl!("made-for-cosmic"),
            Self::NewApps => fl!("new-apps"),
            Self::RecentlyUpdated => fl!("recently-updated"),
            Self::DevelopmentTools => fl!("development-tools"),
            Self::ScientificTools => fl!("scientific-tools"),
            Self::ProductivityApps => fl!("productivity-apps"),
            Self::GraphicsAndPhotographyTools => fl!("graphics-and-photography-tools"),
            Self::SocialNetworkingApps => fl!("social-networking-apps"),
            Self::Games => fl!("games"),
            Self::MusicAndVideoApps => fl!("music-and-video-apps"),
            Self::AppsForLearning => fl!("apps-for-learning"),
            Self::Utilities => fl!("utilities"),
        }
    }

    fn categories(&self) -> &'static [Category] {
        match self {
            Self::DevelopmentTools => &[Category::Development],
            Self::ScientificTools => &[Category::Science],
            Self::ProductivityApps => &[Category::Office],
            Self::GraphicsAndPhotographyTools => &[Category::Graphics],
            Self::SocialNetworkingApps => &[Category::Network],
            Self::Games => &[Category::Game],
            Self::MusicAndVideoApps => &[Category::AudioVideo],
            Self::AppsForLearning => &[Category::Education],
            Self::Utilities => &[Category::Settings, Category::System, Category::Utility],
            _ => &[],
        }
    }
}

pub struct GridMetrics {
    pub cols: usize,
    pub item_width: usize,
    pub column_spacing: u16,
}

impl GridMetrics {
    pub fn new(width: usize, min_width: usize, column_spacing: u16) -> Self {
        let width_m1 = width.checked_sub(min_width).unwrap_or(0);
        let cols_m1 = width_m1 / (min_width + column_spacing as usize);
        let cols = cols_m1 + 1;
        let item_width = width
            .checked_sub(cols_m1 * column_spacing as usize)
            .unwrap_or(0)
            .checked_div(cols)
            .unwrap_or(0);
        Self {
            cols,
            item_width,
            column_spacing,
        }
    }
}

fn package_card_view<'a>(
    info: &'a AppInfo,
    icon_opt: Option<&'a widget::icon::Handle>,
    controls: Vec<Element<'a, Message>>,
    top_controls: Option<Vec<Element<'a, Message>>>,
    spacing: &cosmic_theme::Spacing,
    width: usize,
) -> Element<'a, Message> {
    let height = 20.0 + 28.0 + 32.0 + 3.0 * spacing.space_xxs as f32;
    let top_row_cap = 1 + top_controls
        .as_deref()
        .map(|elements| 1 + elements.len())
        .unwrap_or_default();
    let column = widget::column::with_children(vec![
        widget::row::with_capacity(top_row_cap)
            .push(widget::column::with_children(vec![
                widget::text::body(&info.name)
                    .height(20.0)
                    .width(width as f32 - 180.0)
                    .into(),
                widget::text::caption(&info.summary)
                    .height(28.0)
                    .width(width as f32 - 180.0)
                    .into(),
            ]))
            .push_maybe(top_controls.is_some().then_some(widget::horizontal_space()))
            .extend(top_controls.unwrap_or_default())
            .into(),
        widget::Space::with_height(Length::Fixed(spacing.space_xxs.into())).into(),
        widget::row::with_children(controls)
            .height(32.0)
            .spacing(spacing.space_xs)
            .into(),
    ]);

    let icon: Element<_> = match icon_opt {
        Some(icon) => widget::icon::icon(icon.clone())
            .size(ICON_SIZE_PACKAGE)
            .into(),
        None => widget::Space::with_width(ICON_SIZE_PACKAGE as f32).into(),
    };

    widget::container(
        widget::row()
            .push(icon)
            .push(column)
            .align_y(Alignment::Center)
            .spacing(spacing.space_s),
    )
    .align_y(Alignment::Center)
    .width(width as f32)
    .height(height)
    .padding([spacing.space_xxs, spacing.space_s])
    .class(theme::Container::Card)
    .into()
}

impl Package {
    pub fn grid_metrics(spacing: &cosmic_theme::Spacing, width: usize) -> GridMetrics {
        GridMetrics::new(width, 320 + 2 * spacing.space_s as usize, spacing.space_xxs)
    }

    pub fn card_view<'a>(
        &'a self,
        controls: Vec<Element<'a, Message>>,
        top_controls: Option<Vec<Element<'a, Message>>>,
        spacing: &cosmic_theme::Spacing,
        width: usize,
    ) -> Element<'a, Message> {
        package_card_view(
            &self.info,
            Some(&self.icon),
            controls,
            top_controls,
            spacing,
            width,
        )
    }
}

#[derive(Clone, Debug)]
pub struct SearchResult {
    backend_name: &'static str,
    id: AppId,
    icon_opt: Option<widget::icon::Handle>,
    // Info from selected source
    info: Arc<AppInfo>,
    weight: i64,
}

impl SearchResult {
    pub fn grid_metrics(spacing: &cosmic_theme::Spacing, width: usize) -> GridMetrics {
        GridMetrics::new(width, 240 + 2 * spacing.space_s as usize, spacing.space_xxs)
    }

    pub fn grid_view<'a, F: Fn(usize) -> Message + 'a>(
        results: &'a [Self],
        spacing: cosmic_theme::Spacing,
        width: usize,
        callback: F,
    ) -> Element<'a, Message> {
        let GridMetrics {
            cols,
            item_width,
            column_spacing,
        } = Self::grid_metrics(&spacing, width);

        let mut grid = widget::grid();
        let mut col = 0;
        for (result_i, result) in results.iter().enumerate() {
            if col >= cols {
                grid = grid.insert_row();
                col = 0;
            }
            grid = grid.push(
                widget::mouse_area(result.card_view(&spacing, item_width))
                    .on_press(callback(result_i)),
            );
            col += 1;
        }
        grid.column_spacing(column_spacing)
            .row_spacing(column_spacing)
            .into()
    }

    pub fn card_view<'a>(
        &'a self,
        spacing: &cosmic_theme::Spacing,
        width: usize,
    ) -> Element<'a, Message> {
        widget::container(
            widget::row::with_children(vec![
                match &self.icon_opt {
                    Some(icon) => widget::icon::icon(icon.clone())
                        .size(ICON_SIZE_SEARCH)
                        .into(),
                    None => {
                        widget::Space::with_width(Length::Fixed(ICON_SIZE_SEARCH as f32)).into()
                    }
                },
                widget::column::with_children(vec![
                    widget::text::body(&self.info.name)
                        .height(Length::Fixed(20.0))
                        .into(),
                    widget::text::caption(&self.info.summary)
                        .height(Length::Fixed(28.0))
                        .into(),
                ])
                .into(),
            ])
            .align_y(Alignment::Center)
            .spacing(spacing.space_s),
        )
        .align_y(Alignment::Center)
        .width(Length::Fixed(width as f32))
        .height(Length::Fixed(48.0 + (spacing.space_xxs as f32) * 2.0))
        .padding([spacing.space_xxs, spacing.space_s])
        .class(theme::Container::Card)
        .into()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ScrollContext {
    NavPage,
    ExplorePage,
    SearchResults,
    Selected,
}

impl ScrollContext {
    fn unused_contexts(&self) -> &'static [ScrollContext] {
        // Contexts that can be safely removed when another is active
        match self {
            Self::NavPage => &[Self::Selected, Self::SearchResults, Self::ExplorePage],
            Self::ExplorePage => &[Self::Selected, Self::SearchResults],
            Self::SearchResults => &[Self::Selected],
            Self::Selected => &[],
        }
    }
}

#[derive(Clone, Debug)]
pub struct SelectedSource {
    backend_name: &'static str,
    source_id: String,
    source_name: String,
}

impl SelectedSource {
    fn new(backend_name: &'static str, info: &AppInfo, installed: bool) -> Self {
        SelectedSource {
            backend_name,
            source_id: info.source_id.clone(),
            source_name: if installed {
                fl!("source-installed", source = info.source_name.as_str())
            } else {
                info.source_name.clone()
            },
        }
    }
}

// For use in dropdown widget
impl AsRef<str> for SelectedSource {
    fn as_ref(&self) -> &str {
        &self.source_name
    }
}

#[derive(Clone, Debug)]
pub struct Selected {
    backend_name: &'static str,
    id: AppId,
    icon_opt: Option<widget::icon::Handle>,
    info: Arc<AppInfo>,
    screenshot_images: HashMap<usize, widget::image::Handle>,
    screenshot_shown: usize,
    sources: Vec<SelectedSource>,
}

/// The [`App`] stores application-specific state.
pub struct App {
    core: Core,
    config_handler: Option<cosmic_config::Config>,
    config: Config,
    locale: String,
    app_themes: Vec<String>,
    apps: Arc<Apps>,
    backends: Backends,
    context_page: ContextPage,
    dialog_pages: VecDeque<DialogPage>,
    explore_page_opt: Option<ExplorePage>,
    key_binds: HashMap<KeyBind, Action>,
    nav_model: widget::nav_bar::Model,
    #[cfg(feature = "notify")]
    notification_opt: Option<Arc<Mutex<notify_rust::NotificationHandle>>>,
    pending_operation_id: u64,
    pending_operations: BTreeMap<u64, (Operation, f32)>,
    progress_operations: BTreeSet<u64>,
    complete_operations: BTreeMap<u64, Operation>,
    failed_operations: BTreeMap<u64, (Operation, f32, String)>,
    scrollable_id: widget::Id,
    scroll_views: HashMap<ScrollContext, scrollable::Viewport>,
    search_active: bool,
    search_id: widget::Id,
    search_input: String,
    window_id_opt: Option<window::Id>,
    //TODO: use hashset?
    installed: Option<Vec<(&'static str, Package)>>,
    //TODO: use hashset?
    updates: Option<Vec<(&'static str, Package)>>,
    //TODO: use hashset?
    waiting_installed: Vec<(&'static str, String, AppId)>,
    //TODO: use hashset?
    waiting_updates: Vec<(&'static str, String, AppId)>,
    category_results: Option<(&'static [Category], Vec<SearchResult>)>,
    explore_results: HashMap<ExplorePage, Vec<SearchResult>>,
    installed_results: Option<Vec<SearchResult>>,
    search_results: Option<(String, Vec<SearchResult>)>,
    selected_opt: Option<Selected>,
}

impl App {
    fn open_desktop_id(&self, mut desktop_id: String) -> Task<Message> {
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    if !desktop_id.ends_with(".desktop") {
                        desktop_id.push_str(".desktop");
                    }
                    let xdg_dirs = match xdg::BaseDirectories::with_prefix("applications") {
                        Ok(ok) => ok,
                        Err(err) => {
                            log::warn!("failed to find applications xdg directories: {}", err);
                            return None;
                        }
                    };
                    let path = match xdg_dirs.find_data_file(&desktop_id) {
                        Some(some) => some,
                        None => {
                            log::warn!("failed to find desktop file for {:?}", desktop_id);
                            return None;
                        }
                    };
                    let entry = match freedesktop_entry_parser::parse_entry(&path) {
                        Ok(ok) => ok,
                        Err(err) => {
                            log::warn!("failed to read desktop file {:?}: {}", path, err);
                            return None;
                        }
                    };
                    //TODO: handle Terminal=true
                    let exec = match entry.section("Desktop Entry").attr("Exec") {
                        Some(some) => some,
                        None => {
                            log::warn!("no exec section in {:?}", path);
                            return None;
                        }
                    };
                    //TODO: use libcosmic for loading desktop data
                    Some((exec.to_string(), desktop_id))
                })
                .await
                .unwrap_or(None)
            },
            |result| {
                #[cfg(feature = "desktop")]
                if let Some((exec, desktop_id)) = result {
                    tokio::spawn(async move {
                        cosmic::desktop::spawn_desktop_exec(
                            &exec,
                            Vec::<(&str, &str)>::new(),
                            Some(&desktop_id),
                            false,
                        )
                        .await;
                    });
                }
                action::none()
            },
        )
    }

    fn operation(&mut self, operation: Operation) {
        let id = self.pending_operation_id;
        self.pending_operation_id += 1;
        self.progress_operations.insert(id);
        self.pending_operations.insert(id, (operation, 0.0));
    }

    fn generic_search<F: Fn(&AppId, &AppInfo, bool) -> Option<i64> + Send + Sync>(
        apps: &Apps,
        backends: &Backends,
        filter_map: F,
    ) -> Vec<SearchResult> {
        let mut results: Vec<SearchResult> = apps
            .par_iter()
            .filter_map(|(id, infos)| {
                let mut best_result: Option<SearchResult> = None;
                for AppEntry {
                    backend_name,
                    info,
                    installed,
                } in infos.iter()
                {
                    if let Some(weight) = filter_map(id, info, *installed) {
                        // Skip if best result has lower weight
                        if let Some(prev_result) = &best_result {
                            if prev_result.weight < weight {
                                continue;
                            }
                        }

                        //TODO: put all infos into search result?
                        // Replace best result
                        best_result = Some(SearchResult {
                            backend_name,
                            id: id.clone(),
                            icon_opt: None,
                            info: info.clone(),
                            weight,
                        });
                    }
                }
                best_result
            })
            .collect();
        results.par_sort_unstable_by(|a, b| match a.weight.cmp(&b.weight) {
            cmp::Ordering::Equal => match LANGUAGE_SORTER.compare(&a.info.name, &b.info.name) {
                cmp::Ordering::Equal => LANGUAGE_SORTER.compare(&a.backend_name, &b.backend_name),
                ordering => ordering,
            },
            ordering => ordering,
        });
        // Load only enough icons to show one page of results
        //TODO: load in background
        for result in results.iter_mut().take(MAX_RESULTS) {
            let Some(backend) = backends.get(result.backend_name) else {
                continue;
            };
            let appstream_caches = backend.info_caches();
            let Some(appstream_cache) = appstream_caches
                .iter()
                .find(|x| x.source_id == result.info.source_id)
            else {
                continue;
            };
            result.icon_opt = Some(appstream_cache.icon(&result.info));
        }
        results
    }

    fn categories(&self, categories: &'static [Category]) -> Task<Message> {
        let apps = self.apps.clone();
        let backends = self.backends.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let start = Instant::now();
                    let applet_provide = AppProvide::Id("com.system76.CosmicApplet".to_string());
                    let results =
                        Self::generic_search(&apps, &backends, |_id, info, _installed| {
                            for category in categories {
                                //TODO: this hack makes it easier to add applets to the nav bar
                                if matches!(category, Category::CosmicApplet) {
                                    if info.provides.contains(&applet_provide) {
                                        return Some(-(info.monthly_downloads as i64));
                                    }
                                } else {
                                    //TODO: contains doesn't work due to type mismatch
                                    if info.categories.iter().any(|x| x == category.id()) {
                                        return Some(-(info.monthly_downloads as i64));
                                    }
                                }
                            }
                            None
                        });
                    let duration = start.elapsed();
                    log::info!(
                        "searched for categories {:?} in {:?}, found {} results",
                        categories,
                        duration,
                        results.len()
                    );
                    action::app(Message::CategoryResults(categories, results))
                })
                .await
                .unwrap_or(action::none())
            },
            |x| x,
        )
    }

    fn explore_results(&self, explore_page: ExplorePage) -> Task<Message> {
        let apps = self.apps.clone();
        let backends = self.backends.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    log::info!("start search for {:?}", explore_page);
                    let start = Instant::now();
                    let now = chrono::Utc::now().timestamp();
                    let results = match explore_page {
                        ExplorePage::EditorsChoice => Self::generic_search(&apps, &backends, |id, _info, _installed | {
                            EDITORS_CHOICE
                            .iter()
                            .position(|choice_id| choice_id == &id.normalized())
                            .map(|x| x as i64)
                        }),
                        ExplorePage::PopularApps => Self::generic_search(&apps, &backends, |_id, info, _installed| {
                            Some(-(info.monthly_downloads as i64))
                        }),
                        ExplorePage::MadeForCosmic => {
                            let provide = AppProvide::Id("com.system76.CosmicApplication".to_string());
                            Self::generic_search(&apps, &backends, |_id, info, _installed| {
                                if info.provides.contains(&provide) {
                                    Some(-(info.monthly_downloads as i64))
                                } else {
                                    None
                                }
                            })
                        },
                        ExplorePage::NewApps => Self::generic_search(&apps, &backends, |_id, _info, _installed| {
                            //TODO
                            None
                        }),
                        ExplorePage::RecentlyUpdated => Self::generic_search(&apps, &backends, |id, info, _installed| {
                            // Finds the newest release and sorts from newest to oldest
                            //TODO: appstream release info is often incomplete
                            let mut min_weight = 0;
                            for release in info.releases.iter() {
                                if let Some(timestamp) = release.timestamp {
                                    if timestamp < now {
                                        let weight = -timestamp;
                                        if weight < min_weight {
                                            min_weight = weight;
                                        }
                                    } else {
                                        log::info!("{:?} has release timestamp {} which is past the present {}", id, timestamp, now);
                                    }
                                }
                            }
                            Some(min_weight)
                        }),
                        _ => {
                            let categories = explore_page.categories();
                            Self::generic_search(&apps, &backends, |_id, info, _installed| {
                                for category in categories {
                                    //TODO: contains doesn't work due to type mismatch
                                    if info.categories.iter().any(|x| x == category.id()) {
                                        return Some(-(info.monthly_downloads as i64));
                                    }
                                }
                                None
                            })
                        }
                    };
                    let duration = start.elapsed();
                    log::info!(
                        "searched for {:?} in {:?}, found {} results",
                        explore_page,
                        duration,
                        results.len()
                    );
                    action::app(Message::ExploreResults(explore_page, results))
                })
                .await
                .unwrap_or(action::none())
            },
            |x| x,
        )
    }

    fn installed_results(&self) -> Task<Message> {
        let apps = self.apps.clone();
        let backends = self.backends.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let start = Instant::now();
                    let results = Self::generic_search(&apps, &backends, |id, _info, installed| {
                        if installed {
                            Some(if id.is_system() { -1 } else { 0 })
                        } else {
                            None
                        }
                    });
                    let duration = start.elapsed();
                    log::info!(
                        "searched for installed in {:?}, found {} results",
                        duration,
                        results.len()
                    );
                    action::app(Message::InstalledResults(results))
                })
                .await
                .unwrap_or(action::none())
            },
            |x| x,
        )
    }

    fn search(&self) -> Task<Message> {
        let input = self.search_input.clone();

        // Handle supported URI schemes before trying plain text search
        match reqwest::Url::parse(&input) {
            Ok(url) => match url.scheme() {
                "appstream" => {
                    return self.handle_appstream_url(input, url.path());
                }
                "file" => {
                    return self.handle_file_url(input, url.path());
                }
                "mime" => {
                    // This is a workaround to be able to search for mime handlers, mime is not a real URL scheme
                    return self.handle_mime_url(input, url.path());
                }
                scheme => {
                    log::warn!("unsupported URL scheme {scheme} in {url}");
                }
            },
            Err(_) => {}
        }

        // Also handle standard file paths
        if input.starts_with("/") {
            if Path::new(&input).is_file() {
                return self.handle_file_url(input.clone(), &input);
            }
        }

        let pattern = regex::escape(&input);
        let regex = match regex::RegexBuilder::new(&pattern)
            .case_insensitive(true)
            .build()
        {
            Ok(ok) => ok,
            Err(err) => {
                log::warn!("failed to parse regex {:?}: {}", pattern, err);
                return Task::none();
            }
        };
        let apps = self.apps.clone();
        let backends = self.backends.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let start = Instant::now();
                    let results =
                        Self::generic_search(&apps, &backends, |_id, info, _installed| {
                            //TODO: improve performance
                            let stats_weight = |weight: i64| {
                                //TODO: make sure no overflows
                                (weight << 56) - (info.monthly_downloads as i64)
                            };
                            //TODO: fuzzy match (nucleus-matcher?)
                            match regex.find(&info.name) {
                                Some(mat) => {
                                    if mat.range().start == 0 {
                                        if mat.range().end == info.name.len() {
                                            // Name equals search phrase
                                            Some(stats_weight(0))
                                        } else {
                                            // Name starts with search phrase
                                            Some(stats_weight(1))
                                        }
                                    } else {
                                        // Name contains search phrase
                                        Some(stats_weight(2))
                                    }
                                }
                                None => match regex.find(&info.summary) {
                                    Some(mat) => {
                                        if mat.range().start == 0 {
                                            if mat.range().end == info.summary.len() {
                                                // Summary equals search phrase
                                                Some(stats_weight(3))
                                            } else {
                                                // Summary starts with search phrase
                                                Some(stats_weight(4))
                                            }
                                        } else {
                                            // Summary contains search phrase
                                            Some(stats_weight(5))
                                        }
                                    }
                                    None => match regex.find(&info.description) {
                                        Some(mat) => {
                                            if mat.range().start == 0 {
                                                if mat.range().end == info.summary.len() {
                                                    // Description equals search phrase
                                                    Some(stats_weight(6))
                                                } else {
                                                    // Description starts with search phrase
                                                    Some(stats_weight(7))
                                                }
                                            } else {
                                                // Description contains search phrase
                                                Some(stats_weight(8))
                                            }
                                        }
                                        None => None,
                                    },
                                },
                            }
                        });
                    let duration = start.elapsed();
                    log::info!(
                        "searched for {:?} in {:?}, found {} results",
                        input,
                        duration,
                        results.len()
                    );
                    action::app(Message::SearchResults(input, results, false))
                })
                .await
                .unwrap_or(action::none())
            },
            |x| x,
        )
    }

    fn selected_sources(
        &self,
        backend_name: &'static str,
        id: &AppId,
        info: &AppInfo,
    ) -> Vec<SelectedSource> {
        let mut sources = Vec::new();
        match self.apps.get(&id) {
            Some(infos) => {
                for AppEntry {
                    backend_name,
                    info,
                    installed,
                } in infos.iter()
                {
                    sources.push(SelectedSource::new(backend_name, &info, *installed));
                }
            }
            None => {
                //TODO: warning?
                let installed = self.is_installed(backend_name, &id, &info);
                sources.push(SelectedSource::new(backend_name, &info, installed));
            }
        }
        sources
    }

    fn select(
        &mut self,
        backend_name: &'static str,
        id: AppId,
        icon_opt: Option<widget::icon::Handle>,
        info: Arc<AppInfo>,
    ) -> Task<Message> {
        log::info!(
            "selected {:?} from backend {:?} and source {:?}",
            id,
            backend_name,
            info.source_id
        );
        let sources = self.selected_sources(backend_name, &id, &info);
        self.selected_opt = Some(Selected {
            backend_name,
            id,
            icon_opt,
            info,
            screenshot_images: HashMap::new(),
            screenshot_shown: 0,
            sources,
        });
        self.update_scroll()
    }

    fn scroll_context(&self) -> ScrollContext {
        if self.selected_opt.is_some() {
            ScrollContext::Selected
        } else if self.search_results.is_some() {
            ScrollContext::SearchResults
        } else if self.explore_page_opt.is_some() {
            ScrollContext::ExplorePage
        } else {
            ScrollContext::NavPage
        }
    }

    fn update_scroll(&mut self) -> Task<Message> {
        let scroll_context = self.scroll_context();
        // Clear unused scroll contexts
        for remove_context in scroll_context.unused_contexts() {
            self.scroll_views.remove(remove_context);
        }
        scrollable::scroll_to(
            self.scrollable_id.clone(),
            match self.scroll_views.get(&scroll_context) {
                Some(viewport) => viewport.absolute_offset(),
                None => scrollable::AbsoluteOffset::default(),
            },
        )
    }

    fn update_backends(&mut self, refresh: bool) -> Task<Message> {
        let locale = self.locale.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let start = Instant::now();
                    let backends = backend::backends(&locale, refresh);
                    let duration = start.elapsed();
                    log::info!(
                        "loaded backends {} in {:?}",
                        if refresh {
                            "with refreshing"
                        } else {
                            "without refreshing"
                        },
                        duration
                    );
                    action::app(Message::Backends(backends))
                })
                .await
                .unwrap_or(action::none())
            },
            |x| x,
        )
    }

    fn update_config(&mut self) -> Task<Message> {
        cosmic::command::set_theme(self.config.app_theme.theme())
    }

    fn is_installed(&self, backend_name: &'static str, id: &AppId, info: &AppInfo) -> bool {
        if let Some(installed) = &self.installed {
            for (installed_backend_name, package) in installed {
                if *installed_backend_name == backend_name
                    && package.info.source_id == info.source_id
                {
                    // Simple app match found
                    if &package.id == id {
                        return true;
                    }

                    // Search for matching pkgnames
                    //TODO: also do flatpak refs?
                    if package.id.is_system() && !info.pkgnames.is_empty() {
                        let mut found = true;
                        for pkgname in info.pkgnames.iter() {
                            if !package.info.pkgnames.contains(&pkgname) {
                                found = false;
                                break;
                            }
                        }
                        if found {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    //TODO: run in background
    fn update_apps(&mut self) {
        let start = Instant::now();
        let mut apps = Apps::new();

        let entry_sort = |a: &AppEntry, b: &AppEntry, id: &AppId| {
            // Sort with installed first
            match a.installed.cmp(&b.installed) {
                cmp::Ordering::Equal => {
                    // Sort by highest priority first to lowest priority
                    let a_priority = priority(a.backend_name, &a.info.source_id, id);
                    let b_priority = priority(b.backend_name, &b.info.source_id, id);
                    match a_priority.cmp(&b_priority) {
                        cmp::Ordering::Equal => {
                            match LANGUAGE_SORTER.compare(&a.info.source_id, &b.info.source_id) {
                                cmp::Ordering::Equal => {
                                    LANGUAGE_SORTER.compare(&a.backend_name, &b.backend_name)
                                }
                                ordering => ordering,
                            }
                        }
                        ordering => ordering,
                    }
                }
                ordering => ordering,
            }
        };

        //TODO: par_iter?
        for (backend_name, backend) in self.backends.iter() {
            for appstream_cache in backend.info_caches() {
                for (id, info) in appstream_cache.infos.iter() {
                    let entry = apps.entry(id.clone()).or_insert_with(|| Vec::new());
                    entry.push(AppEntry {
                        backend_name,
                        info: info.clone(),
                        installed: self.is_installed(backend_name, id, &info),
                    });
                    entry.par_sort_unstable_by(|a, b| entry_sort(a, b, id));
                }
            }
        }

        // Manually insert system apps
        if let Some(installed) = &self.installed {
            for (backend_name, package) in installed {
                if package.id.is_system() {
                    let entry = apps.entry(package.id.clone()).or_insert_with(|| Vec::new());
                    entry.push(AppEntry {
                        backend_name,
                        info: package.info.clone(),
                        installed: true,
                    });
                    entry.par_sort_unstable_by(|a, b| entry_sort(a, b, &package.id));
                }
            }
        }

        self.apps = Arc::new(apps);

        // Update selected sources
        {
            let sources_opt = if let Some(selected) = &self.selected_opt {
                Some(self.selected_sources(selected.backend_name, &selected.id, &selected.info))
            } else {
                None
            };
            if let Some(sources) = sources_opt {
                if let Some(selected) = &mut self.selected_opt {
                    selected.sources = sources;
                }
            }
        }

        let duration = start.elapsed();
        log::info!(
            "updated app cache with {} ids in {:?}",
            self.apps.len(),
            duration
        );
    }

    fn update_installed(&self) -> Task<Message> {
        let backends = self.backends.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let mut installed = Vec::new();
                    //TODO: par_iter?
                    for (backend_name, backend) in backends.iter() {
                        let start = Instant::now();
                        match backend.installed() {
                            Ok(packages) => {
                                for package in packages {
                                    installed.push((*backend_name, package));
                                }
                            }
                            Err(err) => {
                                log::error!("failed to list installed: {}", err);
                            }
                        }
                        let duration = start.elapsed();
                        log::info!("loaded installed from {} in {:?}", backend_name, duration);
                    }
                    installed.par_sort_unstable_by(|a, b| {
                        let a_is_system = a.1.id.is_system();
                        let b_is_system = b.1.id.is_system();
                        if a_is_system && !b_is_system {
                            cmp::Ordering::Less
                        } else if b_is_system && !a_is_system {
                            cmp::Ordering::Greater
                        } else {
                            LANGUAGE_SORTER.compare(&a.1.info.name, &b.1.info.name)
                        }
                    });
                    action::app(Message::Installed(installed))
                })
                .await
                .unwrap_or(action::none())
            },
            |x| x,
        )
    }

    fn update_updates(&self) -> Task<Message> {
        let backends = self.backends.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let mut updates = Vec::new();
                    //TODO: par_iter?
                    for (backend_name, backend) in backends.iter() {
                        let start = Instant::now();
                        match backend.updates() {
                            Ok(packages) => {
                                for package in packages {
                                    updates.push((*backend_name, package));
                                }
                            }
                            Err(err) => {
                                log::error!("failed to list updates: {}", err);
                            }
                        }
                        let duration = start.elapsed();
                        log::info!("loaded updates from {} in {:?}", backend_name, duration);
                    }
                    updates.par_sort_unstable_by(|a, b| {
                        if a.1.id.is_system() {
                            cmp::Ordering::Less
                        } else if b.1.id.is_system() {
                            cmp::Ordering::Greater
                        } else {
                            LANGUAGE_SORTER.compare(&a.1.info.name, &b.1.info.name)
                        }
                    });
                    action::app(Message::Updates(updates))
                })
                .await
                .unwrap_or(action::none())
            },
            |x| x,
        )
    }

    fn update_notification(&mut self) -> Task<Message> {
        // Handle closing notification if there are no operations
        if self.pending_operations.is_empty() {
            #[cfg(feature = "notify")]
            if let Some(notification_arc) = self.notification_opt.take() {
                return Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            //TODO: this is nasty
                            let notification_mutex = Arc::try_unwrap(notification_arc).unwrap();
                            let notification = notification_mutex.into_inner().unwrap();
                            notification.close();
                        })
                        .await
                        .unwrap();
                        action::app(Message::MaybeExit)
                    },
                    |x| x,
                );
            }
        }

        Task::none()
    }

    fn handle_appstream_url(&self, input: String, path: &str) -> Task<Message> {
        // Handler for appstream:component-id as described in:
        // https://freedesktop.org/software/appstream/docs/sect-AppStream-Misc-URIHandler.html
        let apps = self.apps.clone();
        let backends = self.backends.clone();
        let component_id = AppId::new(path.trim_start_matches('/'));
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let start = Instant::now();
                    let results =
                        Self::generic_search(&apps, &backends, |id, _info, _installed| {
                            //TODO: fuzzy search with lower weight?
                            if id == &component_id {
                                Some(0)
                            } else {
                                None
                            }
                        });
                    let duration = start.elapsed();
                    log::info!(
                        "searched for ID {:?} in {:?}, found {} results",
                        component_id,
                        duration,
                        results.len()
                    );
                    action::app(Message::SearchResults(input, results, true))
                })
                .await
                .unwrap_or(action::none())
            },
            |x| x,
        )
    }

    fn handle_file_url(&self, input: String, path: &str) -> Task<Message> {
        let path = path.to_string();
        let backends = self.backends.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let start = Instant::now();
                    let mut packages = Vec::new();
                    for (backend_name, backend) in backends.iter() {
                        match backend.file_packages(&path) {
                            Ok(backend_packages) => {
                                for package in backend_packages {
                                    packages.push((backend_name, package));
                                }
                            }
                            Err(err) => {
                                log::warn!(
                                    "failed to file {:?} using backend {:?}: {}",
                                    path,
                                    backend_name,
                                    err
                                );
                            }
                        }
                    }
                    let duration = start.elapsed();
                    log::info!(
                        "loaded file {:?} in {:?}, found {} packages",
                        path,
                        duration,
                        packages.len()
                    );

                    //TODO: store the resolved packages somewhere
                    let mut results = Vec::with_capacity(packages.len());
                    for (backend_name, package) in packages {
                        results.push(SearchResult {
                            backend_name,
                            id: package.id,
                            icon_opt: Some(package.icon),
                            info: package.info,
                            weight: 0,
                        });
                    }
                    action::app(Message::SearchResults(input, results, true))
                })
                .await
                .unwrap_or(action::none())
            },
            |x| x,
        )
    }

    fn handle_mime_url(&self, input: String, path: &str) -> Task<Message> {
        let apps = self.apps.clone();
        let backends = self.backends.clone();
        let mime = path.trim_matches('/').to_string();
        let provide = AppProvide::MediaType(mime.clone());
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let start = Instant::now();
                    let results =
                        Self::generic_search(&apps, &backends, |_id, info, _installed| {
                            //TODO: monthly downloads as weight?
                            if info.provides.contains(&provide) {
                                Some(-(info.monthly_downloads as i64))
                            } else {
                                None
                            }
                        });
                    let duration = start.elapsed();
                    log::info!(
                        "searched for mime {:?} in {:?}, found {} results",
                        mime,
                        duration,
                        results.len()
                    );
                    action::app(Message::SearchResults(input, results, false))
                })
                .await
                .unwrap_or(action::none())
            },
            |x| x,
        )
    }

    fn update_title(&mut self) -> Task<Message> {
        if let Some(window_id) = &self.window_id_opt {
            self.set_window_title(fl!("app-name"), window_id.clone())
        } else {
            Task::none()
        }
    }

    fn operations(&self) -> Element<Message> {
        let cosmic_theme::Spacing {
            space_xs, space_m, ..
        } = theme::active().cosmic().spacing;

        let mut children = Vec::new();

        //TODO: get height from theme?
        let progress_bar_height = Length::Fixed(4.0);

        if !self.pending_operations.is_empty() {
            let mut section = widget::settings::section().title(fl!("pending"));
            for (_id, (op, progress)) in self.pending_operations.iter().rev() {
                section = section.add(widget::column::with_children(vec![
                    widget::progress_bar(0.0..=100.0, *progress)
                        .height(progress_bar_height)
                        .into(),
                    widget::Space::with_height(space_xs).into(),
                    widget::text(op.pending_text(*progress as i32)).into(),
                ]));
            }
            children.push(section.into());
        }

        if !self.failed_operations.is_empty() {
            let mut section = widget::settings::section().title(fl!("failed"));
            for (_id, (op, progress, error)) in self.failed_operations.iter().rev() {
                section = section.add(widget::column::with_children(vec![
                    widget::text(op.pending_text(*progress as i32)).into(),
                    widget::text(error).into(),
                ]));
            }
            children.push(section.into());
        }

        if !self.complete_operations.is_empty() {
            let mut section = widget::settings::section().title(fl!("complete"));
            for (_id, op) in self.complete_operations.iter().rev() {
                section = section.add(widget::text(op.completed_text()));
            }
            children.push(section.into());
        }

        if children.is_empty() {
            children.push(widget::text::body(fl!("no-operations")).into());
        }

        widget::column::with_children(children)
            .spacing(space_m)
            .into()
    }

    fn settings(&self) -> Element<Message> {
        let app_theme_selected = match self.config.app_theme {
            AppTheme::Dark => 1,
            AppTheme::Light => 2,
            AppTheme::System => 0,
        };
        widget::settings::view_column(vec![widget::settings::section()
            .title(fl!("appearance"))
            .add(
                widget::settings::item::builder(fl!("theme")).control(widget::dropdown(
                    &self.app_themes,
                    Some(app_theme_selected),
                    move |index| {
                        Message::AppTheme(match index {
                            1 => AppTheme::Dark,
                            2 => AppTheme::Light,
                            _ => AppTheme::System,
                        })
                    },
                )),
            )
            .into()])
        .into()
    }

    fn release_notes(&self, index: usize) -> Element<Message> {
        let (version, date, summary, url) = {
            self.updates
                .as_deref()
                .and_then(|updates| updates.get(index).map(|(_, package)| package))
                .and_then(|selected| {
                    selected.info.releases.last().map(|latest| {
                        (
                            &*latest.version,
                            latest.timestamp,
                            latest.description.to_owned(),
                            latest.url.as_deref(),
                        )
                    })
                })
                .unwrap_or(("", None, None, None))
        };
        let cosmic_theme::Spacing { space_s, .. } = theme::active().cosmic().spacing;
        widget::column::with_capacity(3)
            .push(
                widget::column::with_capacity(2)
                    .push(widget::text::title4(format!(
                        "{} {}",
                        fl!("latest-version"),
                        version
                    )))
                    .push_maybe(
                        date.and_then(|secs| {
                            chrono::DateTime::from_timestamp(secs, 0).map(|dt| {
                                dt.with_timezone(&chrono::Local)
                                    .format("%Y-%m-%d")
                                    .to_string()
                            })
                        })
                        .map(widget::text),
                    ),
            )
            .push(widget::scrollable(widget::text(
                summary.unwrap_or_else(|| fl!("no-description")),
            )))
            .push_maybe(url.map(widget::text))
            .width(Length::Fill)
            .spacing(space_s)
            .into()
    }

    fn view_responsive(&self, size: Size) -> Element<Message> {
        let spacing = theme::active().cosmic().spacing;
        let cosmic_theme::Spacing {
            space_l,
            space_m,
            space_s,
            space_xs,
            space_xxs,
            space_xxxs,
            ..
        } = spacing;
        let grid_width = (size.width - 2.0 * space_s as f32).floor().max(0.0) as usize;
        match &self.selected_opt {
            Some(selected) => {
                //TODO: more efficient checks
                let mut waiting_refresh = false;
                for (backend_name, source_id, package_id) in self
                    .waiting_installed
                    .iter()
                    .chain(self.waiting_updates.iter())
                {
                    if backend_name == &selected.backend_name
                        && source_id == &selected.info.source_id
                        && package_id == &selected.id
                    {
                        waiting_refresh = true;
                        break;
                    }
                }
                let is_installed =
                    self.is_installed(selected.backend_name, &selected.id, &selected.info);
                let mut update_opt = None;
                if let Some(updates) = &self.updates {
                    for (backend_name, package) in updates {
                        if backend_name == &selected.backend_name
                            && &package.info.source_id == &selected.info.source_id
                            && &package.id == &selected.id
                        {
                            update_opt = Some(Message::Operation(
                                OperationKind::Update,
                                backend_name,
                                package.id.clone(),
                                package.info.clone(),
                            ));
                            break;
                        }
                    }
                }
                let mut progress_opt = None;
                for (_id, (op, progress)) in self.pending_operations.iter() {
                    if op.backend_name == selected.backend_name
                        && op
                            .infos
                            .iter()
                            .any(|info| info.source_id == selected.info.source_id)
                        && op
                            .package_ids
                            .iter()
                            .any(|package_id| package_id == &selected.id)
                    {
                        progress_opt = Some(*progress);
                        break;
                    }
                }

                let mut selected_source = None;
                for (i, source) in selected.sources.iter().enumerate() {
                    if source.backend_name == selected.backend_name
                        && &source.source_id == &selected.info.source_id
                    {
                        selected_source = Some(i);
                        break;
                    }
                }

                let mut column = widget::column::with_capacity(2)
                    .padding([0, space_s])
                    .spacing(space_m)
                    .width(Length::Fill);
                column = column.push(
                    //TODO: describe where we are going back to
                    widget::button::text(fl!("back"))
                        .leading_icon(icon_cache_handle("go-previous-symbolic", 16))
                        .on_press(Message::SelectNone),
                );
                let mut buttons = Vec::with_capacity(2);
                if let Some(progress) = progress_opt {
                    //TODO: get height from theme?
                    buttons.push(
                        widget::progress_bar(0.0..=100.0, progress)
                            .height(Length::Fixed(4.0))
                            .into(),
                    )
                } else if waiting_refresh {
                    // Do not show buttons while waiting for refresh
                } else if is_installed {
                    //TODO: what if there are multiple desktop IDs?
                    if let Some(desktop_id) = selected.info.desktop_ids.first() {
                        buttons.push(
                            widget::button::suggested(fl!("open"))
                                .on_press(Message::OpenDesktopId(desktop_id.clone()))
                                .into(),
                        );
                    }
                    if let Some(update) = update_opt {
                        buttons.push(
                            widget::button::standard(fl!("update"))
                                .on_press(update)
                                .into(),
                        );
                    }
                    if !selected.id.is_system() {
                        buttons.push(
                            widget::button::destructive(fl!("uninstall"))
                                .on_press(Message::DialogPage(DialogPage::Uninstall(
                                    selected.backend_name,
                                    selected.id.clone(),
                                    selected.info.clone(),
                                )))
                                .into(),
                        );
                    }
                } else {
                    buttons.push(
                        widget::button::suggested(fl!("install"))
                            .on_press(Message::Operation(
                                OperationKind::Install,
                                selected.backend_name,
                                selected.id.clone(),
                                selected.info.clone(),
                            ))
                            .into(),
                    )
                }
                column = column.push(
                    widget::row::with_children(vec![
                        match &selected.icon_opt {
                            Some(icon) => widget::icon::icon(icon.clone())
                                .size(ICON_SIZE_DETAILS)
                                .into(),
                            None => {
                                widget::Space::with_width(Length::Fixed(ICON_SIZE_DETAILS as f32))
                                    .into()
                            }
                        },
                        widget::column::with_children(vec![
                            widget::text::title2(&selected.info.name).into(),
                            widget::text(&selected.info.summary).into(),
                            widget::Space::with_height(Length::Fixed(space_s.into())).into(),
                            widget::row::with_children(buttons).spacing(space_xs).into(),
                        ])
                        .into(),
                    ])
                    .align_y(Alignment::Center)
                    .spacing(space_m),
                );

                let sources_widget = widget::column::with_children(vec![if selected.sources.len()
                    == 1
                {
                    widget::text(selected.sources[0].as_ref()).into()
                } else {
                    widget::dropdown(&selected.sources, selected_source, Message::SelectedSource)
                        .into()
                }])
                .align_x(Alignment::Center)
                .width(Length::Fill);
                let developers_widget = widget::column::with_children(vec![
                    if selected.info.developer_name.is_empty() {
                        widget::text::heading(fl!(
                            "app-developers",
                            app = selected.info.name.as_str()
                        ))
                        .into()
                    } else {
                        widget::text::heading(&selected.info.developer_name).into()
                    },
                    widget::text::body(fl!("developer")).into(),
                ])
                .align_x(Alignment::Center)
                .width(Length::Fill);
                let downloads_widget = (selected.info.monthly_downloads > 0).then(|| {
                    widget::column::with_children(vec![
                        widget::text::heading(selected.info.monthly_downloads.to_string()).into(),
                        //TODO: description of what this means?
                        widget::text::body(fl!("monthly-downloads")).into(),
                    ])
                    .align_x(Alignment::Center)
                    .width(Length::Fill)
                });
                if grid_width < 416 {
                    let size = 4 + if downloads_widget.is_some() { 3 } else { 0 };
                    let downloads_widget_space = downloads_widget
                        .is_some()
                        .then(widget::divider::horizontal::default);
                    column = column.push(
                        widget::column::with_capacity(size)
                            .push(widget::divider::horizontal::default())
                            .push(sources_widget)
                            .push(widget::divider::horizontal::default())
                            .push(developers_widget)
                            .push(widget::divider::horizontal::default())
                            .push_maybe(downloads_widget)
                            .push_maybe(downloads_widget_space)
                            .spacing(space_xxs),
                    );
                } else {
                    let row_size = 4 + if downloads_widget.is_some() { 2 } else { 0 };
                    let downloads_widget_space = downloads_widget
                        .is_some()
                        .then(|| widget::divider::vertical::default().height(Length::Fixed(32.0)));
                    column = column.push(
                        widget::column::with_children(vec![
                            widget::divider::horizontal::default().into(),
                            widget::row::with_capacity(row_size)
                                .push(sources_widget)
                                .push(
                                    widget::divider::vertical::default()
                                        .height(Length::Fixed(32.0)),
                                )
                                .push(developers_widget)
                                .push_maybe(downloads_widget_space)
                                .push_maybe(downloads_widget)
                                .align_y(Alignment::Center)
                                .into(),
                            widget::divider::horizontal::default().into(),
                        ])
                        .spacing(space_xxs),
                    );
                }
                //TODO: proper image scroller
                if let Some(screenshot) = selected.info.screenshots.get(selected.screenshot_shown) {
                    //TODO: get proper image dimensions
                    let image_height = Length::Fixed(480.0);
                    let mut row = widget::row::with_capacity(3).align_y(Alignment::Center);
                    {
                        let mut button = widget::button::icon(
                            widget::icon::from_name("go-previous-symbolic").size(16),
                        );
                        let index = selected.screenshot_shown.checked_sub(1).unwrap_or_else(|| {
                            selected
                                .info
                                .screenshots
                                .len()
                                .checked_sub(1)
                                .unwrap_or_default()
                        });
                        if index != selected.screenshot_shown {
                            button = button.on_press(Message::SelectedScreenshotShown(index));
                        }
                        row = row.push(button);
                    }
                    let image_element = if let Some(image) =
                        selected.screenshot_images.get(&selected.screenshot_shown)
                    {
                        widget::container(widget::image(image.clone()).height(image_height))
                            .center_x(Length::Fill)
                            .into()
                    } else {
                        widget::Space::new(Length::Fill, image_height).into()
                    };
                    row = row.push(
                        widget::column::with_children(vec![
                            image_element,
                            widget::text::caption(&screenshot.caption).into(),
                        ])
                        .align_x(Alignment::Center),
                    );
                    {
                        let mut button = widget::button::icon(
                            widget::icon::from_name("go-next-symbolic").size(16),
                        );
                        let index =
                            if selected.screenshot_shown + 1 == selected.info.screenshots.len() {
                                0
                            } else {
                                selected.screenshot_shown + 1
                            };
                        if index != selected.screenshot_shown {
                            button = button.on_press(Message::SelectedScreenshotShown(index));
                        }
                        row = row.push(button);
                    }
                    column = column.push(row);
                }
                column = column.push(widget::text::body(&selected.info.description));

                for release in selected.info.releases.iter() {
                    let mut release_col = widget::column::with_capacity(2).spacing(space_xxxs);
                    //TODO: translate
                    release_col = release_col
                        .push(widget::text::title4(format!("Version {}", release.version)));
                    if let Some(timestamp) = release.timestamp {
                        if let Some(utc) =
                            chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0)
                        {
                            let local = chrono::DateTime::<chrono::Local>::from(utc);
                            release_col = release_col.push(widget::text::body(format!(
                                "{}",
                                local.format("%b %-d, %-Y")
                            )));
                        }
                    }
                    if let Some(description) = &release.description {
                        release_col = release_col.push(widget::text::body(description));
                    }
                    column = column.push(release_col);
                    //TODO: show more releases, or make sure this is the latest?
                    break;
                }

                if let Some(license) = &selected.info.license_opt {
                    let mut license_col = widget::column::with_capacity(2).spacing(space_xxxs);
                    license_col = license_col.push(widget::text::title4(fl!("licenses")));
                    match spdx::Expression::parse_mode(license, spdx::ParseMode::LAX) {
                        Ok(expr) => {
                            for item in expr.requirements() {
                                match &item.req.license {
                                    spdx::LicenseItem::Spdx { id, .. } => {
                                        license_col =
                                            license_col.push(widget::text::body(id.full_name));
                                    }
                                    spdx::LicenseItem::Other { lic_ref, .. } => {
                                        let mut parts = lic_ref.splitn(2, '=');
                                        parts.next();
                                        if let Some(url) = parts.next() {
                                            license_col = license_col.push(
                                                widget::button::link(fl!("proprietary"))
                                                    .on_press(Message::LaunchUrl(url.to_string()))
                                                    .padding(0),
                                            )
                                        } else {
                                            license_col = license_col
                                                .push(widget::text::body(fl!("proprietary")));
                                        }
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            license_col = license_col.push(widget::text::body(license));
                            license_col = license_col.push(widget::text::body(format!("{}", err)));
                        }
                    }
                    column = column.push(license_col);
                }

                if !selected.info.urls.is_empty() {
                    let mut url_items = Vec::with_capacity(selected.info.urls.len());
                    for app_url in &selected.info.urls {
                        let (name, url) = match app_url {
                            AppUrl::BugTracker(url) => (fl!("bug-tracker"), url),
                            AppUrl::Contact(url) => (fl!("contact"), url),
                            AppUrl::Donation(url) => (fl!("donation"), url),
                            AppUrl::Faq(url) => (fl!("faq"), url),
                            AppUrl::Help(url) => (fl!("help"), url),
                            AppUrl::Homepage(url) => (fl!("homepage"), url),
                            AppUrl::Translate(url) => (fl!("translate"), url),
                        };
                        url_items.push(
                            widget::button::link(name)
                                .on_press(Message::LaunchUrl(url.to_string()))
                                .padding(0)
                                .into(),
                        );
                    }
                    if grid_width < 416 {
                        column = column
                            .push(widget::column::with_children(url_items).spacing(space_xxxs));
                    } else {
                        column = column.push(
                            widget::row::with_children(url_items)
                                .spacing(space_s)
                                .align_y(Alignment::Center),
                        );
                    }
                }

                column.into()
            }
            None => match &self.search_results {
                Some((input, results)) => {
                    //TODO: paging or dynamic load
                    let results_len = cmp::min(results.len(), MAX_RESULTS);

                    let mut column = widget::column::with_capacity(2)
                        .padding([0, space_s])
                        .spacing(space_xxs)
                        .width(Length::Fill);
                    //TODO: back button?
                    if results.is_empty() {
                        column =
                            column.push(widget::text(fl!("no-results", search = input.as_str())));
                    }
                    column = column.push(SearchResult::grid_view(
                        &results[..results_len],
                        spacing,
                        grid_width,
                        |result_i| Message::SelectSearchResult(result_i),
                    ));
                    column.into()
                }
                None => match self
                    .nav_model
                    .active_data::<NavPage>()
                    .map_or(NavPage::default(), |nav_page| *nav_page)
                {
                    NavPage::Explore => {
                        match self.explore_page_opt {
                            Some(explore_page) => {
                                let mut column = widget::column::with_capacity(3)
                                    .padding([0, space_s])
                                    .spacing(space_xxs)
                                    .width(Length::Fill);
                                column = column.push(
                                    widget::button::text(NavPage::Explore.title())
                                        .leading_icon(icon_cache_handle("go-previous-symbolic", 16))
                                        .on_press(Message::ExplorePage(None)),
                                );
                                column = column.push(widget::text::title4(explore_page.title()));
                                //TODO: ensure explore_page matches
                                match self.explore_results.get(&explore_page) {
                                    Some(results) => {
                                        //TODO: paging or dynamic load
                                        let results_len = cmp::min(results.len(), MAX_RESULTS);

                                        if results.is_empty() {
                                            //TODO: no results message?
                                        }
                                        column = column.push(SearchResult::grid_view(
                                            &results[..results_len],
                                            spacing,
                                            grid_width,
                                            move |result_i| {
                                                Message::SelectExploreResult(explore_page, result_i)
                                            },
                                        ));
                                    }
                                    None => {
                                        //TODO: loading message?
                                    }
                                }
                                column.into()
                            }
                            None => {
                                let explore_pages = ExplorePage::all();
                                let mut column =
                                    widget::column::with_capacity(explore_pages.len() * 2)
                                        .padding([0, space_s])
                                        .spacing(space_xxs)
                                        .width(Length::Fill);
                                for explore_page in explore_pages.iter() {
                                    //TODO: ensure explore_page matches
                                    match self.explore_results.get(&explore_page) {
                                        Some(results) if !results.is_empty() => {
                                            let GridMetrics { cols, .. } =
                                                SearchResult::grid_metrics(&spacing, grid_width);

                                            let max_results = match cols {
                                                1 => 4,
                                                2 => 8,
                                                3 => 9,
                                                _ => cols * 2,
                                            };

                                            //TODO: adjust results length based on app size?
                                            let results_len = cmp::min(results.len(), max_results);

                                            column = column.push(widget::row::with_children(vec![
                                                widget::text::title4(explore_page.title()).into(),
                                                widget::horizontal_space().into(),
                                                widget::button::text(fl!("see-all"))
                                                    .trailing_icon(icon_cache_handle(
                                                        "go-next-symbolic",
                                                        16,
                                                    ))
                                                    .on_press(Message::ExplorePage(Some(
                                                        *explore_page,
                                                    )))
                                                    .into(),
                                            ]));

                                            column = column.push(SearchResult::grid_view(
                                                &results[..results_len],
                                                spacing,
                                                grid_width,
                                                |result_i| {
                                                    Message::SelectExploreResult(
                                                        *explore_page,
                                                        result_i,
                                                    )
                                                },
                                            ));
                                        }
                                        _ => {}
                                    }
                                }
                                column.into()
                            }
                        }
                    }
                    NavPage::Installed => {
                        let mut column = widget::column::with_capacity(3)
                            .padding([0, space_s])
                            .spacing(space_xxs)
                            .width(Length::Fill);
                        column = column.push(widget::text::title2(NavPage::Installed.title()));
                        match &self.installed_results {
                            Some(installed) => {
                                if installed.is_empty() {
                                    column =
                                        column.push(widget::text(fl!("no-installed-applications")));
                                }

                                let GridMetrics {
                                    cols,
                                    item_width,
                                    column_spacing,
                                } = Package::grid_metrics(&spacing, grid_width);
                                let mut grid = widget::grid();
                                let mut col = 0;
                                for (installed_i, result) in installed.iter().enumerate() {
                                    if col >= cols {
                                        grid = grid.insert_row();
                                        col = 0;
                                    }
                                    let mut buttons = Vec::with_capacity(1);
                                    if let Some(desktop_id) = result.info.desktop_ids.first() {
                                        buttons.push(
                                            widget::button::standard(fl!("open"))
                                                .on_press(Message::OpenDesktopId(
                                                    desktop_id.clone(),
                                                ))
                                                .into(),
                                        );
                                    } else {
                                        buttons.push(
                                            widget::Space::with_height(Length::Shrink).into(),
                                        );
                                    }
                                    grid = grid.push(
                                        widget::mouse_area(package_card_view(
                                            &result.info,
                                            result.icon_opt.as_ref(),
                                            buttons,
                                            None,
                                            &spacing,
                                            item_width,
                                        ))
                                        .on_press(Message::SelectInstalled(installed_i)),
                                    );
                                    col += 1;
                                }
                                column = column.push(
                                    grid.column_spacing(column_spacing)
                                        .row_spacing(column_spacing),
                                );
                            }
                            None => {
                                //TODO: loading message?
                            }
                        }
                        column.into()
                    }
                    //TODO: reduce duplication
                    NavPage::Updates => {
                        let mut column = widget::column::with_capacity(3)
                            .padding([0, space_s])
                            .spacing(space_xxs)
                            .width(Length::Fill);
                        match &self.updates {
                            Some(updates) => {
                                if updates.is_empty() {
                                    column = column
                                        .push(widget::text::title2(NavPage::Updates.title()))
                                        .push(
                                            widget::column::with_capacity(2)
                                                .spacing(space_s)
                                                .padding([space_l, 0])
                                                .width(Length::Fill)
                                                .align_x(Alignment::Center)
                                                .push(widget::text::body(fl!("no-updates")))
                                                .push(
                                                    widget::button::standard(fl!(
                                                        "check-for-updates"
                                                    ))
                                                    .on_press(Message::CheckUpdates),
                                                ),
                                        );
                                } else {
                                    column = column.push(widget::flex_row(vec![
                                        widget::text::title2(NavPage::Updates.title()).into(),
                                        widget::horizontal_space().width(Length::Fill).into(),
                                        widget::row::with_capacity(2)
                                            .spacing(space_xxs)
                                            .push(
                                                widget::button::standard(fl!("check-for-updates"))
                                                    .on_press(Message::CheckUpdates),
                                            )
                                            .push(
                                                widget::button::standard(fl!("update-all"))
                                                    .on_press(Message::UpdateAll),
                                            )
                                            .into(),
                                    ]));
                                }

                                let GridMetrics {
                                    cols,
                                    item_width,
                                    column_spacing,
                                } = Package::grid_metrics(&spacing, grid_width);
                                let mut grid = widget::grid();
                                let mut col = 0;
                                for (updates_i, (backend_name, package)) in
                                    updates.iter().enumerate()
                                {
                                    let mut waiting_refresh = false;
                                    for (other_backend_name, source_id, package_id) in self
                                        .waiting_installed
                                        .iter()
                                        .chain(self.waiting_updates.iter())
                                    {
                                        if other_backend_name == backend_name
                                            && source_id == &package.info.source_id
                                            && package_id == &package.id
                                        {
                                            waiting_refresh = true;
                                            break;
                                        }
                                    }
                                    let mut progress_opt = None;
                                    for (_id, (op, progress)) in self.pending_operations.iter() {
                                        if &op.backend_name == backend_name
                                            && op.infos.iter().any(|info| {
                                                info.source_id == package.info.source_id
                                            })
                                            && op
                                                .package_ids
                                                .iter()
                                                .any(|package_id| package_id == &package.id)
                                        {
                                            progress_opt = Some(*progress);
                                            break;
                                        }
                                    }
                                    let controls = if let Some(progress) = progress_opt {
                                        vec![widget::progress_bar(0.0..=100.0, progress)
                                            .height(Length::Fixed(4.0))
                                            .into()]
                                    } else if waiting_refresh {
                                        vec![]
                                    } else {
                                        vec![widget::button::standard(fl!("update"))
                                            .on_press(Message::Operation(
                                                OperationKind::Update,
                                                backend_name,
                                                package.id.clone(),
                                                package.info.clone(),
                                            ))
                                            .into()]
                                    };
                                    let top_controls = Some(vec![widget::button::icon(
                                        widget::icon::from_name("help-info-symbolic"),
                                    )
                                    .on_press(Message::ToggleContextPage(
                                        ContextPage::ReleaseNotes(
                                            updates_i,
                                            package.info.name.clone(),
                                        ),
                                    ))
                                    .into()]);
                                    if col >= cols {
                                        grid = grid.insert_row();
                                        col = 0;
                                    }
                                    grid = grid.push(
                                        widget::mouse_area(package.card_view(
                                            controls,
                                            top_controls,
                                            &spacing,
                                            item_width,
                                        ))
                                        .on_press(Message::SelectUpdates(updates_i)),
                                    );
                                    col += 1;
                                }
                                column = column.push(
                                    grid.column_spacing(column_spacing)
                                        .row_spacing(column_spacing),
                                );
                            }
                            None => {
                                column = column
                                    .push(widget::text::title2(NavPage::Updates.title()))
                                    .push(
                                        widget::column::with_capacity(2)
                                            .spacing(space_s)
                                            .padding([space_l, 0])
                                            .width(Length::Fill)
                                            .align_x(Alignment::Center)
                                            /*.push(
                                                widget::progress_bar(0.0..=100.0, progress)
                                                    .height(Length::Fixed(4.0))
                                                    .width(Length::Fixed(446.0)),
                                            )*/
                                            .push(widget::text(fl!("checking-for-updates"))),
                                    );
                            }
                        }
                        column.into()
                    }
                    //TODO: reduce duplication
                    nav_page => {
                        let mut column = widget::column::with_capacity(2)
                            .padding([0, space_s])
                            .spacing(space_xxs)
                            .width(Length::Fill);
                        column = column.push(widget::text::title2(nav_page.title()));
                        //TODO: ensure category matches?
                        match &self.category_results {
                            Some((_, results)) => {
                                //TODO: paging or dynamic load
                                let results_len = cmp::min(results.len(), MAX_RESULTS);

                                if results.is_empty() {
                                    //TODO: no results message?
                                }

                                column = column.push(SearchResult::grid_view(
                                    &results[..results_len],
                                    spacing,
                                    grid_width,
                                    |result_i| Message::SelectCategoryResult(result_i),
                                ));
                            }
                            None => {
                                //TODO: loading message?
                            }
                        }
                        column.into()
                    }
                },
            },
        }
    }
}

/// Implement [`Application`] to integrate with COSMIC.
impl Application for App {
    /// Multithreaded async executor to use with the app.
    type Executor = executor::multi::Executor;

    /// Argument received
    type Flags = Flags;

    /// Message type specific to our [`App`].
    type Message = Message;

    /// The unique application ID to supply to the window manager.
    const APP_ID: &'static str = "com.system76.CosmicStore";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    /// Creates the application, and optionally emits command on initialize.
    fn init(core: Core, flags: Self::Flags) -> (Self, Task<Self::Message>) {
        let locale = sys_locale::get_locale().unwrap_or_else(|| {
            log::warn!("failed to get system locale, falling back to en-US");
            String::from("en-US")
        });

        let app_themes = vec![fl!("match-desktop"), fl!("dark"), fl!("light")];

        let mut nav_model = widget::nav_bar::Model::default();
        for &nav_page in NavPage::all() {
            let id = nav_model
                .insert()
                .icon(nav_page.icon())
                .text(nav_page.title())
                .data::<NavPage>(nav_page)
                .id();
            if nav_page == NavPage::default() {
                //TODO: save last page?
                nav_model.activate(id);
            }
        }

        //TODO: is this going to be set correctly?
        let window_id_opt = core.main_window_id();

        let mut app = App {
            core,
            config_handler: flags.config_handler,
            config: flags.config,
            locale,
            app_themes,
            apps: Arc::new(Apps::new()),
            backends: Backends::new(),
            context_page: ContextPage::Settings,
            dialog_pages: VecDeque::new(),
            explore_page_opt: None,
            key_binds: key_binds(),
            nav_model,
            #[cfg(feature = "notify")]
            notification_opt: None,
            pending_operation_id: 0,
            pending_operations: BTreeMap::new(),
            progress_operations: BTreeSet::new(),
            complete_operations: BTreeMap::new(),
            failed_operations: BTreeMap::new(),
            scrollable_id: widget::Id::unique(),
            scroll_views: HashMap::new(),
            search_active: false,
            search_id: widget::Id::unique(),
            search_input: String::new(),
            window_id_opt,
            installed: None,
            updates: None,
            waiting_installed: Vec::new(),
            waiting_updates: Vec::new(),
            category_results: None,
            explore_results: HashMap::new(),
            installed_results: None,
            search_results: None,
            selected_opt: None,
        };

        if let Some(subcommand) = flags.subcommand_opt {
            // Search for term
            app.search_active = true;
            app.search_input = subcommand;
        }

        let command = Task::batch([app.update_title(), app.update_backends(false)]);
        (app, command)
    }

    fn nav_model(&self) -> Option<&widget::nav_bar::Model> {
        Some(&self.nav_model)
    }

    #[cfg(feature = "single-instance")]
    fn dbus_activation(&mut self, msg: cosmic::dbus_activation::Message) -> Task<Message> {
        //TODO: parse msg
        log::info!("{:?}", msg);
        if self.window_id_opt.is_none() {
            let (window_id, task) = window::open(window::Settings {
                min_size: Some(Size::new(360.0, 180.0)),
                decorations: false,
                exit_on_close_request: false,
                ..Default::default()
            });
            self.window_id_opt = Some(window_id);
            return task.map(|_id| action::none());
        }
        Task::none()
    }

    fn on_app_exit(&mut self) -> Option<Message> {
        Some(Message::WindowClose)
    }

    fn on_escape(&mut self) -> Task<Message> {
        if self.core.window.show_context {
            // Close context drawer if open
            self.core.window.show_context = false;
        } else if self.search_active {
            // Close search if open
            self.search_active = false;
            if self.search_results.take().is_some() {
                return self.update_scroll();
            }
        }
        Task::none()
    }

    fn on_nav_select(&mut self, id: widget::nav_bar::Id) -> Task<Message> {
        self.category_results = None;
        self.explore_page_opt = None;
        self.search_active = false;
        self.search_results = None;
        self.selected_opt = None;
        self.nav_model.activate(id);
        let mut commands = Vec::with_capacity(2);
        self.scroll_views.clear();
        commands.push(self.update_scroll());
        if let Some(categories) = self
            .nav_model
            .active_data::<NavPage>()
            .and_then(|nav_page| nav_page.categories())
        {
            commands.push(self.categories(categories));
        }
        match self.nav_model.active_data::<NavPage>() {
            Some(NavPage::Updates) => {
                // Refresh when going to updates page
                commands.push(self.update(Message::CheckUpdates));
            }
            _ => {}
        }
        Task::batch(commands)
    }

    /// Handle application events here.
    fn update(&mut self, message: Self::Message) -> Task<Message> {
        // Helper for updating config values efficiently
        macro_rules! config_set {
            ($name: ident, $value: expr) => {
                match &self.config_handler {
                    Some(config_handler) => {
                        match paste::paste! { self.config.[<set_ $name>](config_handler, $value) } {
                            Ok(_) => {}
                            Err(err) => {
                                log::warn!(
                                    "failed to save config {:?}: {}",
                                    stringify!($name),
                                    err
                                );
                            }
                        }
                    }
                    None => {
                        self.config.$name = $value;
                        log::warn!(
                            "failed to save config {:?}: no config handler",
                            stringify!($name)
                        );
                    }
                }
            };
        }

        match message {
            Message::AppTheme(app_theme) => {
                config_set!(app_theme, app_theme);
                return self.update_config();
            }
            Message::Backends(backends) => {
                self.backends = backends;
                return Task::batch([self.update_installed(), self.update_updates()]);
            }
            Message::CategoryResults(categories, results) => {
                self.category_results = Some((categories, results));
                return self.update_scroll();
            }
            Message::CheckUpdates => {
                //TODO: this only checks updates if they have already been checked
                if self.updates.take().is_some() {
                    if self.pending_operations.is_empty() {
                        return self.update_backends(true);
                    } else {
                        log::warn!("cannot check for updates, operations are in progress");
                    }
                } else {
                    log::warn!("already checking for updates");
                }
            }
            Message::Config(config) => {
                if config != self.config {
                    log::info!("update config");
                    //TODO: update syntax theme by clearing tabs, only if needed
                    self.config = config;
                    return self.update_config();
                }
            }
            Message::DialogCancel => {
                self.dialog_pages.pop_front();
            }
            Message::DialogConfirm => match self.dialog_pages.pop_front() {
                Some(DialogPage::Uninstall(backend_name, id, info)) => {
                    return self.update(Message::Operation(
                        OperationKind::Uninstall,
                        backend_name,
                        id,
                        info,
                    ));
                }
                _ => {}
            },
            Message::DialogPage(dialog_page) => {
                self.dialog_pages.push_back(dialog_page);
            }
            Message::ExplorePage(explore_page_opt) => {
                self.explore_page_opt = explore_page_opt;
                return self.update_scroll();
            }
            Message::ExploreResults(explore_page, results) => {
                self.explore_results.insert(explore_page, results);
            }
            Message::Installed(installed) => {
                self.installed = Some(installed);
                self.waiting_installed.clear();

                self.update_apps();
                let mut commands = Vec::new();
                //TODO: search not done if item is selected because that would clear selection
                if self.search_active && !self.selected_opt.is_some() {
                    // Update search if active
                    commands.push(self.search());
                }
                commands.push(self.installed_results());
                for explore_page in ExplorePage::all() {
                    commands.push(self.explore_results(*explore_page));
                }
                return Task::batch(commands);
            }
            Message::InstalledResults(installed_results) => {
                self.installed_results = Some(installed_results);
            }
            Message::Key(modifiers, key, text) => {
                for (key_bind, action) in self.key_binds.iter() {
                    if key_bind.matches(modifiers, &key) {
                        return self.update(action.message());
                    }
                }

                // Uncaptured keys with only shift modifiers go to the search box
                if !modifiers.logo()
                    && !modifiers.control()
                    && !modifiers.alt()
                    && matches!(key, Key::Character(_))
                {
                    if let Some(text) = text {
                        self.search_active = true;
                        self.search_input.push_str(&text);
                        return Task::batch([
                            widget::text_input::focus(self.search_id.clone()),
                            self.search(),
                        ]);
                    }
                }
            }
            Message::LaunchUrl(url) => match open::that_detached(&url) {
                Ok(()) => {}
                Err(err) => {
                    log::warn!("failed to open {:?}: {}", url, err);
                }
            },
            Message::MaybeExit => {
                if self.window_id_opt.is_none() && self.pending_operations.is_empty() {
                    // Exit if window is closed and there are no pending operations
                    process::exit(0);
                }
            }
            #[cfg(feature = "notify")]
            Message::Notification(notification) => {
                self.notification_opt = Some(notification);
            }
            Message::OpenDesktopId(desktop_id) => {
                return self.open_desktop_id(desktop_id);
            }
            Message::Operation(kind, backend_name, package_id, info) => {
                self.operation(Operation {
                    kind,
                    backend_name,
                    package_ids: vec![package_id],
                    infos: vec![info],
                });
            }
            Message::PendingComplete(id) => {
                if let Some((op, _)) = self.pending_operations.remove(&id) {
                    for (package_id, info) in op.package_ids.iter().zip(op.infos.iter()) {
                        self.waiting_installed.push((
                            op.backend_name,
                            info.source_id.clone(),
                            package_id.clone(),
                        ));
                        self.waiting_updates.push((
                            op.backend_name,
                            info.source_id.clone(),
                            package_id.clone(),
                        ));
                    }
                    self.complete_operations.insert(id, op);
                }
                // Close progress notification if all relavent operations are finished
                if self.pending_operations.is_empty() {
                    self.progress_operations.clear();
                }
                return Task::batch([
                    self.update_notification(),
                    self.update_installed(),
                    self.update_updates(),
                ]);
            }
            Message::PendingDismiss => {
                self.progress_operations.clear();
            }
            Message::PendingError(id, err) => {
                log::warn!("operation {id} failed: {err}");
                if let Some((op, progress)) = self.pending_operations.remove(&id) {
                    self.failed_operations.insert(id, (op, progress, err));
                    self.dialog_pages.push_back(DialogPage::FailedOperation(id));
                }
                // Close progress notification if all relavent operations are finished
                if self.pending_operations.is_empty() {
                    self.progress_operations.clear();
                }
            }
            Message::PendingProgress(id, new_progress) => {
                if let Some((_, progress)) = self.pending_operations.get_mut(&id) {
                    *progress = new_progress;
                }
                return self.update_notification();
            }
            Message::ScrollView(viewport) => {
                self.scroll_views.insert(self.scroll_context(), viewport);
            }
            Message::SearchActivate => {
                self.search_active = true;
                return widget::text_input::focus(self.search_id.clone());
            }
            Message::SearchClear => {
                self.search_active = false;
                self.search_input.clear();
                if self.search_results.take().is_some() {
                    return self.update_scroll();
                }
            }
            Message::SearchInput(input) => {
                if input != self.search_input {
                    self.search_input = input;
                    // This performs live search
                    if !self.search_input.is_empty() {
                        return self.search();
                    }
                }
            }
            Message::SearchResults(input, results, auto_select) => {
                if input == self.search_input {
                    // Clear selected item so search results can be shown
                    self.selected_opt = None;
                    if auto_select && results.len() == 1 {
                        // This drops update_scroll's command, it will be done again later
                        let _ = self.select(
                            results[0].backend_name,
                            results[0].id.clone(),
                            results[0].icon_opt.clone(),
                            results[0].info.clone(),
                        );
                    }
                    self.search_results = Some((input, results));
                    return self.update_scroll();
                } else {
                    log::warn!(
                        "received {} results for {:?} after search changed to {:?}",
                        results.len(),
                        input,
                        self.search_input
                    );
                }
            }
            Message::SearchSubmit(_search_input) => {
                if !self.search_input.is_empty() {
                    return self.search();
                }
            }
            Message::Select(backend_name, id, icon, info) => {
                return self.select(backend_name, id, icon, info);
            }
            Message::SelectInstalled(result_i) => {
                if let Some(results) = &self.installed_results {
                    match results.get(result_i) {
                        Some(result) => {
                            return self.select(
                                result.backend_name,
                                result.id.clone(),
                                result.icon_opt.clone(),
                                result.info.clone(),
                            )
                        }
                        None => {
                            log::error!("failed to find installed result with index {}", result_i);
                        }
                    }
                }
            }
            Message::SelectUpdates(updates_i) => {
                if let Some(updates) = &self.updates {
                    match updates
                        .get(updates_i)
                        .map(|(backend_name, package)| (backend_name, package.clone()))
                    {
                        Some((backend_name, package)) => {
                            return self.select(
                                backend_name,
                                package.id,
                                Some(package.icon),
                                package.info,
                            );
                        }
                        None => {
                            log::error!("failed to find updates package with index {}", updates_i);
                        }
                    }
                }
            }
            Message::SelectNone => {
                self.selected_opt = None;
                return self.update_scroll();
            }
            Message::SelectCategoryResult(result_i) => {
                if let Some((_, results)) = &self.category_results {
                    match results.get(result_i) {
                        Some(result) => {
                            return self.select(
                                result.backend_name,
                                result.id.clone(),
                                result.icon_opt.clone(),
                                result.info.clone(),
                            )
                        }
                        None => {
                            log::error!("failed to find category result with index {}", result_i);
                        }
                    }
                }
            }
            Message::SelectExploreResult(explore_page, result_i) => {
                if let Some(results) = self.explore_results.get(&explore_page) {
                    match results.get(result_i) {
                        Some(result) => {
                            return self.select(
                                result.backend_name,
                                result.id.clone(),
                                result.icon_opt.clone(),
                                result.info.clone(),
                            )
                        }
                        None => {
                            log::error!(
                                "failed to find {:?} result with index {}",
                                explore_page,
                                result_i
                            );
                        }
                    }
                }
            }
            Message::SelectSearchResult(result_i) => {
                if let Some((_input, results)) = &self.search_results {
                    match results.get(result_i) {
                        Some(result) => {
                            return self.select(
                                result.backend_name,
                                result.id.clone(),
                                result.icon_opt.clone(),
                                result.info.clone(),
                            )
                        }
                        None => {
                            log::error!("failed to find search result with index {}", result_i);
                        }
                    }
                }
            }
            Message::SelectedScreenshot(i, url, data) => {
                if let Some(selected) = &mut self.selected_opt {
                    if let Some(screenshot) = selected.info.screenshots.get(i) {
                        if screenshot.url == url {
                            selected
                                .screenshot_images
                                .insert(i, widget::image::Handle::from_bytes(data));
                        }
                    }
                }
            }
            Message::SelectedScreenshotShown(i) => {
                if let Some(selected) = &mut self.selected_opt {
                    selected.screenshot_shown = i;
                }
            }
            Message::SelectedSource(i) => {
                //TODO: show warnings if anything is not found?
                let mut next_ids = None;
                if let Some(selected) = &self.selected_opt {
                    if let Some(source) = selected.sources.get(i) {
                        next_ids = Some((
                            source.backend_name,
                            source.source_id.clone(),
                            selected.id.clone(),
                        ));
                    }
                }

                //TODO: can this be simplified?
                if let Some((backend_name, source_id, id)) = next_ids {
                    if let Some(backend) = self.backends.get(backend_name) {
                        for appstream_cache in backend.info_caches() {
                            if appstream_cache.source_id == source_id {
                                if let Some(info) = appstream_cache.infos.get(&id) {
                                    return self.select(
                                        backend_name,
                                        id,
                                        Some(appstream_cache.icon(info)),
                                        info.clone(),
                                    );
                                }
                            }
                        }
                    }

                    // Search for installed item if appstream cache had no info (for system packages)
                    if let Some(installed) = &self.installed {
                        for (installed_backend_name, package) in installed {
                            if installed_backend_name == &backend_name
                                && &package.info.source_id == &source_id
                                && &package.id == &id
                            {
                                return self.select(
                                    backend_name,
                                    id,
                                    Some(package.icon.clone()),
                                    package.info.clone(),
                                );
                            }
                        }
                    }
                }
            }
            Message::SystemThemeModeChange(_theme_mode) => {
                return self.update_config();
            }
            Message::ToggleContextPage(context_page) => {
                //TODO: ensure context menus are closed
                if self.context_page == context_page {
                    self.core.window.show_context = !self.core.window.show_context;
                } else {
                    self.context_page = context_page;
                    self.core.window.show_context = true;
                }
            }
            Message::UpdateAll => {
                if let Some(updates) = &self.updates {
                    let mut ops = HashMap::with_capacity(self.backends.len());
                    for (backend_name, package) in updates.iter() {
                        let op = ops.entry(*backend_name).or_insert_with(|| Operation {
                            kind: OperationKind::Update,
                            backend_name,
                            package_ids: Vec::new(),
                            infos: Vec::new(),
                        });
                        op.package_ids.push(package.id.clone());
                        op.infos.push(package.info.clone());
                    }
                    for (_backend_name, op) in ops {
                        self.operation(op);
                    }
                }
            }
            Message::Updates(updates) => {
                self.updates = Some(updates);
                self.waiting_updates.clear();
            }
            Message::WindowClose => {
                if let Some(window_id) = self.window_id_opt.take() {
                    return Task::batch([
                        window::close(window_id),
                        Task::perform(async move { action::app(Message::MaybeExit) }, |x| x),
                    ]);
                }
            }
            Message::WindowNew => match env::current_exe() {
                Ok(exe) => match process::Command::new(&exe).spawn() {
                    Ok(_child) => {}
                    Err(err) => {
                        log::error!("failed to execute {:?}: {}", exe, err);
                    }
                },
                Err(err) => {
                    log::error!("failed to get current executable path: {}", err);
                }
            },
        }

        Task::none()
    }

    fn context_drawer(&self) -> Option<context_drawer::ContextDrawer<Message>> {
        if !self.core.window.show_context {
            return None;
        }

        Some(match &self.context_page {
            ContextPage::Operations => context_drawer::context_drawer(
                self.operations(),
                Message::ToggleContextPage(ContextPage::Operations),
            )
            .title(fl!("operations")),
            ContextPage::Settings => context_drawer::context_drawer(
                self.settings(),
                Message::ToggleContextPage(ContextPage::Settings),
            )
            .title(fl!("settings")),
            ContextPage::ReleaseNotes(i, app_name) => context_drawer::context_drawer(
                self.release_notes(*i),
                Message::ToggleContextPage(ContextPage::ReleaseNotes(*i, app_name.clone())),
            )
            .title(app_name),
        })
    }

    fn dialog(&self) -> Option<Element<Message>> {
        let dialog_page = match self.dialog_pages.front() {
            Some(some) => some,
            None => return None,
        };

        let dialog = match dialog_page {
            DialogPage::FailedOperation(id) => {
                //TODO: try next dialog page (making sure index is used by Dialog messages)?
                let (operation, _, err) = self.failed_operations.get(id)?;

                let (title, body) = operation.failed_dialog(&err);
                widget::dialog()
                    .title(title)
                    .body(body)
                    .icon(widget::icon::from_name("dialog-error").size(64))
                    //TODO: retry action
                    .primary_action(
                        widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
                    )
            }
            DialogPage::Uninstall(_backend_name, _id, info) => widget::dialog()
                .title(fl!("uninstall-app", name = info.name.as_str()))
                .body(fl!("uninstall-app-warning", name = info.name.as_str()))
                .icon(widget::icon::from_name(Self::APP_ID).size(64))
                .primary_action(
                    widget::button::destructive(fl!("uninstall")).on_press(Message::DialogConfirm),
                )
                .secondary_action(
                    widget::button::standard(fl!("cancel")).on_press(Message::DialogCancel),
                ),
        };

        Some(dialog.into())
    }

    fn footer(&self) -> Option<Element<Message>> {
        if self.progress_operations.is_empty() {
            return None;
        }

        let cosmic_theme::Spacing {
            space_xxs,
            space_xs,
            space_s,
            ..
        } = theme::active().cosmic().spacing;

        let mut title = String::new();
        let mut total_progress = 0.0;
        let mut count = 0;
        for (_id, (op, progress)) in self.pending_operations.iter() {
            if title.is_empty() {
                title = op.pending_text(*progress as i32);
            }
            total_progress += progress;
            count += 1;
        }
        let running = count;
        // Adjust the progress bar so it does not jump around when operations finish
        for id in self.progress_operations.iter() {
            if self.complete_operations.contains_key(&id) {
                total_progress += 100.0;
                count += 1;
            }
        }
        let finished = count - running;
        total_progress /= count as f32;
        if running > 1 {
            if finished > 0 {
                title = fl!(
                    "operations-running-finished",
                    running = running,
                    finished = finished,
                    percent = (total_progress as i32)
                );
            } else {
                title = fl!(
                    "operations-running",
                    running = running,
                    percent = (total_progress as i32)
                );
            }
        }

        //TODO: get height from theme?
        let progress_bar_height = Length::Fixed(4.0);
        let progress_bar =
            widget::progress_bar(0.0..=100.0, total_progress).height(progress_bar_height);

        let container = widget::layer_container(widget::column::with_children(vec![
            progress_bar.into(),
            widget::Space::with_height(space_xs).into(),
            widget::text::body(title).into(),
            widget::Space::with_height(space_s).into(),
            widget::row::with_children(vec![
                widget::button::link(fl!("details"))
                    .on_press(Message::ToggleContextPage(ContextPage::Operations))
                    .padding(0)
                    .trailing_icon(true)
                    .into(),
                widget::horizontal_space().into(),
                widget::button::standard(fl!("dismiss"))
                    .on_press(Message::PendingDismiss)
                    .into(),
            ])
            .align_y(Alignment::Center)
            .into(),
        ]))
        .padding([space_xxs, space_xs])
        .layer(cosmic_theme::Layer::Primary);

        Some(container.into())
    }

    fn header_start(&self) -> Vec<Element<Message>> {
        vec![if self.search_active {
            widget::text_input::search_input("", &self.search_input)
                .width(Length::Fixed(240.0))
                .id(self.search_id.clone())
                .on_clear(Message::SearchClear)
                .on_input(Message::SearchInput)
                .on_submit(Message::SearchSubmit)
                .into()
        } else {
            widget::button::icon(widget::icon::from_name("system-search-symbolic"))
                .on_press(Message::SearchActivate)
                .padding(8)
                .into()
        }]
    }

    /// Creates a view after each update.
    fn view(&self) -> Element<Self::Message> {
        let content: Element<_> = widget::responsive(move |mut size| {
            size.width = size.width.min(MAX_GRID_WIDTH);
            widget::scrollable(
                widget::container(
                    widget::container(self.view_responsive(size)).max_width(MAX_GRID_WIDTH),
                )
                .align_x(Alignment::Center),
            )
            .id(self.scrollable_id.clone())
            .on_scroll(Message::ScrollView)
            .into()
        })
        .into();

        // Uncomment to debug layout:
        //content.explain(cosmic::iced::Color::WHITE)
        content
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        struct ConfigSubscription;
        struct ThemeSubscription;

        let mut subscriptions = vec![
            event::listen_with(|event, status, _window_id| match event {
                Event::Keyboard(KeyEvent::KeyPressed {
                    key,
                    modifiers,
                    text,
                    ..
                }) => match status {
                    event::Status::Ignored => Some(Message::Key(modifiers, key, text)),
                    event::Status::Captured => None,
                },
                Event::Window(WindowEvent::CloseRequested) => Some(Message::WindowClose),
                _ => None,
            }),
            cosmic_config::config_subscription(
                TypeId::of::<ConfigSubscription>(),
                Self::APP_ID.into(),
                CONFIG_VERSION,
            )
            .map(|update| {
                if !update.errors.is_empty() {
                    log::debug!("errors loading config: {:?}", update.errors);
                }
                Message::SystemThemeModeChange(update.config)
            }),
            cosmic_config::config_subscription::<_, cosmic_theme::ThemeMode>(
                TypeId::of::<ThemeSubscription>(),
                cosmic_theme::THEME_MODE_ID.into(),
                cosmic_theme::ThemeMode::version(),
            )
            .map(|update| {
                if !update.errors.is_empty() {
                    log::debug!("errors loading theme mode: {:?}", update.errors);
                }
                Message::SystemThemeModeChange(update.config)
            }),
        ];

        if !self.pending_operations.is_empty() {
            #[cfg(feature = "logind")]
            {
                struct InhibitSubscription;
                subscriptions.push(Subscription::run_with_id(
                    TypeId::of::<InhibitSubscription>(),
                    stream::channel(1, move |_msg_tx| async move {
                        let _inhibits = logind::inhibit().await;
                        pending().await
                    }),
                ));
            }

            #[cfg(feature = "notify")]
            if self.window_id_opt.is_none() {
                struct NotificationSubscription;
                subscriptions.push(Subscription::run_with_id(
                    TypeId::of::<NotificationSubscription>(),
                    stream::channel(1, move |msg_tx| async move {
                        let msg_tx = Arc::new(tokio::sync::Mutex::new(msg_tx));
                        tokio::task::spawn_blocking(move || match notify_rust::Notification::new()
                            .summary(&fl!("notification-in-progress"))
                            .auto_icon()
                            .show()
                        {
                            Ok(notification) => {
                                let _ = futures::executor::block_on(async {
                                    msg_tx
                                        .lock()
                                        .await
                                        .send(Message::Notification(Arc::new(Mutex::new(
                                            notification,
                                        ))))
                                        .await
                                });
                            }
                            Err(err) => {
                                log::warn!("failed to create notification: {}", err);
                            }
                        })
                        .await
                        .unwrap();

                        pending().await
                    }),
                ));
            }
        }

        for (id, (op, _)) in self.pending_operations.iter() {
            //TODO: use recipe?
            let id = *id;
            let backend_opt = self.backends.get(op.backend_name).map(|x| x.clone());
            let op = op.clone();
            subscriptions.push(Subscription::run_with_id(
                id,
                stream::channel(16, move |msg_tx| async move {
                    let msg_tx = Arc::new(tokio::sync::Mutex::new(msg_tx));
                    let res = match backend_opt {
                        Some(backend) => {
                            let msg_tx = msg_tx.clone();
                            tokio::task::spawn_blocking(move || {
                                backend
                                    .operation(
                                        &op,
                                        Box::new(move |progress| -> () {
                                            let _ = futures::executor::block_on(async {
                                                msg_tx
                                                    .lock()
                                                    .await
                                                    .send(Message::PendingProgress(id, progress))
                                                    .await
                                            });
                                        }),
                                    )
                                    .map_err(|err| err.to_string())
                            })
                            .await
                            .unwrap()
                        }
                        None => Err(format!("backend {:?} not found", op.backend_name)),
                    };

                    match res {
                        Ok(()) => {
                            let _ = msg_tx.lock().await.send(Message::PendingComplete(id)).await;
                        }
                        Err(err) => {
                            let _ = msg_tx
                                .lock()
                                .await
                                .send(Message::PendingError(id, err.to_string()))
                                .await;
                        }
                    }
                    pending().await
                }),
            ));
        }

        if let Some(selected) = &self.selected_opt {
            for (screenshot_i, screenshot) in selected.info.screenshots.iter().enumerate() {
                let url = screenshot.url.clone();
                subscriptions.push(Subscription::run_with_id(
                    url.clone(),
                    stream::channel(16, move |mut msg_tx| async move {
                        log::info!("fetch screenshot {}", url);
                        match reqwest::get(&url).await {
                            Ok(response) => match response.bytes().await {
                                Ok(bytes) => {
                                    log::info!(
                                        "fetched screenshot from {}: {} bytes",
                                        url,
                                        bytes.len()
                                    );
                                    let _ = msg_tx
                                        .send(Message::SelectedScreenshot(
                                            screenshot_i,
                                            url,
                                            bytes.to_vec(),
                                        ))
                                        .await;
                                }
                                Err(err) => {
                                    log::warn!("failed to read screenshot from {}: {}", url, err);
                                }
                            },
                            Err(err) => {
                                log::warn!("failed to request screenshot from {}: {}", url, err);
                            }
                        }
                        pending().await
                    }),
                ));
            }
        }

        Subscription::batch(subscriptions)
    }
}
