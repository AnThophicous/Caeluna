//! Pure state and geometry for Rouch's native application gallery.
//!
//! The gallery deliberately knows nothing about Flatpak processes, network
//! access or rendering.  A Linux backend supplies [`FlatpakApp`] values and
//! reports whether they came from a live Flathub query or a local cache; a
//! renderer only needs to sample [`AppStore`] and [`AppStoreLayout`].

use crate::windowing::{Point, Rect};

/// Number of application cards placed in one row of the gallery.
pub const GRID_COLUMNS: usize = 4;

/// The default gallery window width.
pub const WINDOW_WIDTH: i32 = 980;

/// The default gallery window height.
pub const WINDOW_HEIGHT: i32 = 650;

/// Width of the category rail.
pub const SIDEBAR_WIDTH: i32 = 190;

/// Height of the toolbar containing the title and search field.
pub const TOOLBAR_HEIGHT: i32 = 68;

/// Width of one application card.
pub const CARD_WIDTH: i32 = 174;

/// Height of one application card.
pub const CARD_HEIGHT: i32 = 198;

/// Space between application cards.
pub const CARD_GAP: i32 = 16;

/// Height of one category button.
pub const CATEGORY_HEIGHT: i32 = 36;

/// Gap between category buttons.
pub const CATEGORY_GAP: i32 = 4;

/// A user-facing category supported by the gallery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AppCategory {
    /// Every application in the current catalogue.
    #[default]
    All,
    AudioVideo,
    Development,
    Education,
    Games,
    Graphics,
    Network,
    Office,
    Science,
    Utility,
    System,
    /// An application whose raw Flatpak categories are missing or unknown.
    Other,
}

impl AppCategory {
    /// Categories shown by the gallery, in stable visual order.
    pub const ALL: [Self; 12] = [
        Self::All,
        Self::AudioVideo,
        Self::Development,
        Self::Education,
        Self::Games,
        Self::Graphics,
        Self::Network,
        Self::Office,
        Self::Science,
        Self::Utility,
        Self::System,
        Self::Other,
    ];

    /// The concise label used by a category button.
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All Apps",
            Self::AudioVideo => "Audio & Video",
            Self::Development => "Development",
            Self::Education => "Education",
            Self::Games => "Games",
            Self::Graphics => "Graphics",
            Self::Network => "Internet",
            Self::Office => "Office",
            Self::Science => "Science",
            Self::Utility => "Utilities",
            Self::System => "System",
            Self::Other => "Other",
        }
    }

    /// The freedesktop/Flatpak category spelling used for persistence.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::AudioVideo => "audio-video",
            Self::Development => "development",
            Self::Education => "education",
            Self::Games => "games",
            Self::Graphics => "graphics",
            Self::Network => "network",
            Self::Office => "office",
            Self::Science => "science",
            Self::Utility => "utility",
            Self::System => "system",
            Self::Other => "other",
        }
    }

    /// Map a raw Flatpak/freedesktop category name to a known category.
    ///
    /// Unknown names are intentionally returned as `None`; the caller can
    /// still retain the original metadata instead of pretending to know its
    /// meaning.
    pub fn from_flatpak_category(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase().replace(['_', '-', ' '], "");
        match normalized.as_str() {
            "audiovideo" | "audio" | "video" => Some(Self::AudioVideo),
            "development" | "develop" => Some(Self::Development),
            "education" => Some(Self::Education),
            "game" | "games" => Some(Self::Games),
            "graphics" => Some(Self::Graphics),
            "network" | "internet" => Some(Self::Network),
            "office" => Some(Self::Office),
            "science" => Some(Self::Science),
            "utility" | "utilities" => Some(Self::Utility),
            "system" | "settings" => Some(Self::System),
            _ => None,
        }
    }

    /// Map a raw category to a visible category, using `Other` honestly for
    /// values that are not part of the gallery's small navigation vocabulary.
    pub fn from_name(value: &str) -> Self {
        Self::from_flatpak_category(value).unwrap_or(Self::Other)
    }

    /// Whether this category includes a raw Flatpak category list.
    pub fn matches(self, categories: &[String]) -> bool {
        if self == Self::All {
            return true;
        }
        if categories.is_empty() {
            return self == Self::Other;
        }

        let mut known = categories
            .iter()
            .filter_map(|category| Self::from_flatpak_category(category));
        match self {
            Self::Other => known.count() == 0,
            category => known.any(|candidate| candidate == category),
        }
    }
}

/// Metadata supplied by Flatpak or retained by the local cache.
///
/// Optional fields stay optional because older Flatpak versions and offline
/// caches do not necessarily expose every piece of AppStream metadata.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FlatpakMetadata {
    /// Reverse-DNS application ID, such as `org.gnome.Calculator`.
    pub app_id: String,
    /// Display name.
    pub name: String,
    /// One-line summary.
    pub summary: String,
    /// Longer description, when available.
    pub description: String,
    /// AppStream version.
    pub version: Option<String>,
    /// Flatpak branch, normally `stable`.
    pub branch: Option<String>,
    /// Flatpak architecture, such as `x86_64`.
    pub architecture: Option<String>,
    /// Remote that supplied the entry, normally `flathub`.
    pub origin: Option<String>,
    /// Runtime ID, when exposed by the query.
    pub runtime: Option<String>,
    /// SPDX license string, when exposed by the query.
    pub license: Option<String>,
    /// Project homepage, when exposed by the query.
    pub homepage: Option<String>,
    /// Icon URL or local icon reference, when exposed by the query.
    pub icon: Option<String>,
    /// Raw AppStream/freedesktop categories.
    pub categories: Vec<String>,
    /// Download/install size when known.
    pub size_bytes: Option<u64>,
}

impl FlatpakMetadata {
    /// Construct the minimum honest metadata needed to render a card.
    pub fn new(app_id: impl Into<String>, name: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            name: name.into(),
            summary: summary.into(),
            ..Self::default()
        }
    }

    /// A compact label for a card when the remote did not provide a version.
    pub fn version_label(&self) -> &str {
        self.version.as_deref().unwrap_or("Version unavailable")
    }

    /// Whether this metadata belongs to a selected category.
    pub fn in_category(&self, category: AppCategory) -> bool {
        category.matches(&self.categories)
    }
}

/// A gallery entry: metadata plus local installation state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatpakApp {
    /// Flatpak/AppStream metadata.
    pub metadata: FlatpakMetadata,
    /// True when the app is installed for the current user or system.
    pub installed: bool,
    /// True when the backend observed a newer remote version.
    pub update_available: bool,
}

impl FlatpakApp {
    /// Construct an available app with the minimum metadata.
    pub fn new(app_id: impl Into<String>, name: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            metadata: FlatpakMetadata::new(app_id, name, summary),
            installed: false,
            update_available: false,
        }
    }

    /// Wrap complete metadata as an available app.
    pub fn from_metadata(metadata: FlatpakMetadata) -> Self {
        Self {
            metadata,
            installed: false,
            update_available: false,
        }
    }

    /// The Flatpak ID, convenient for action routing.
    pub fn app_id(&self) -> &str {
        &self.metadata.app_id
    }

    /// The card's display name.
    pub fn name(&self) -> &str {
        &self.metadata.name
    }

    /// The card's summary.
    pub fn summary(&self) -> &str {
        &self.metadata.summary
    }

    /// The action currently appropriate for the app.
    pub fn primary_action(&self) -> AppAction {
        if !self.installed {
            AppAction::Install
        } else if self.update_available {
            AppAction::Update
        } else {
            AppAction::Open
        }
    }

    /// Whether the app matches a search query. Search covers the fields a
    /// user can reasonably recognize without querying the server again.
    pub fn matches_query(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        query.is_empty()
            || [
                self.metadata.app_id.as_str(),
                self.metadata.name.as_str(),
                self.metadata.summary.as_str(),
                self.metadata.description.as_str(),
            ]
            .iter()
            .any(|field| field.to_lowercase().contains(&query))
            || self
                .metadata
                .categories
                .iter()
                .any(|category| category.to_lowercase().contains(&query))
    }

    /// Whether this app belongs to a category.
    pub fn in_category(&self, category: AppCategory) -> bool {
        self.metadata.in_category(category)
    }
}

/// Action a card can expose to the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppAction {
    Install,
    Update,
    Open,
}

/// State of the most recent catalogue load.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CatalogState {
    /// A backend request is in flight or about to be started.
    #[default]
    Loading,
    /// A live or cached catalogue contains one or more entries.
    Ready,
    /// The load completed successfully but contained no entries.
    Empty,
    /// A load failed for a reason other than a known offline/unavailable
    /// condition. The message is safe to show in diagnostics/UI.
    Error(String),
    /// Flatpak or its remote could not be reached. Cached entries may still be
    /// available and are intentionally not hidden by this state.
    Offline,
}

impl CatalogState {
    /// True for a state that can show an offline badge.
    pub fn is_offline(&self) -> bool {
        matches!(self, Self::Offline)
    }

    /// True while the first backend response has not settled.
    pub fn is_loading(&self) -> bool {
        matches!(self, Self::Loading)
    }

    /// True when the state carries an error message.
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }

    /// Error text, if this is an error state.
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Error(message) => Some(message),
            _ => None,
        }
    }
}

/// Where a catalogue snapshot came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogSource {
    /// Current Flatpak/Flathub command output.
    Live,
    /// A previously persisted or in-memory catalogue.
    Cache,
    /// Entries discovered only from local installed state or an integration
    /// supplied local seed.
    Local,
}

/// The result of hit-testing the gallery surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppStoreHit {
    None,
    Search,
    Category(usize),
    Card(usize),
}

/// Rectangles sampled by a renderer for one frame of the gallery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppStoreLayout {
    pub window: Rect,
    pub sidebar: Rect,
    pub toolbar: Rect,
    pub search: Rect,
    pub content: Rect,
    pub categories: Vec<Rect>,
    pub cards: Vec<Rect>,
}

impl AppStoreLayout {
    /// Return the card index under a point, if any.
    pub fn card_at(&self, point: Point) -> Option<usize> {
        self.cards.iter().position(|rect| rect.contains_point(point))
    }

    /// Return the category-button index under a point, if any.
    pub fn category_at(&self, point: Point) -> Option<usize> {
        self.categories.iter().position(|rect| rect.contains_point(point))
    }

    /// Hit-test the whole gallery in front-to-back order.
    pub fn hit_test(&self, point: Point) -> AppStoreHit {
        if let Some(index) = self.card_at(point) {
            return AppStoreHit::Card(index);
        }
        if self.search.contains_point(point) {
            return AppStoreHit::Search;
        }
        if let Some(index) = self.category_at(point) {
            return AppStoreHit::Category(index);
        }
        AppStoreHit::None
    }
}

/// Compute the default gallery window, centered and clamped to the work area.
pub fn window_rect(work_area: Rect) -> Rect {
    let width = WINDOW_WIDTH.min(work_area.size.width.saturating_sub(24)).max(0);
    let height = WINDOW_HEIGHT.min(work_area.size.height.saturating_sub(24)).max(0);
    Rect::new(
        work_area.origin.x + (work_area.size.width - width) / 2,
        work_area.origin.y + (work_area.size.height - height) / 2,
        width,
        height,
    )
}

/// Compute gallery geometry for a window and visible card count.
pub fn layout(window: Rect, visible_count: usize) -> AppStoreLayout {
    let sidebar = Rect::new(
        window.origin.x,
        window.origin.y + TOOLBAR_HEIGHT,
        SIDEBAR_WIDTH.min(window.size.width.max(0)),
        (window.size.height - TOOLBAR_HEIGHT).max(0),
    );
    let toolbar = Rect::new(
        window.origin.x,
        window.origin.y,
        window.size.width.max(0),
        TOOLBAR_HEIGHT,
    );
    let content = Rect::new(
        sidebar.right(),
        toolbar.bottom(),
        (window.right() - sidebar.right()).max(0),
        (window.bottom() - toolbar.bottom()).max(0),
    );
    let search_width = 300.min((toolbar.size.width - 32).max(0));
    let search = Rect::new(
        toolbar.right() - search_width - 16,
        toolbar.origin.y + 18,
        search_width,
        32.min(toolbar.size.height.max(0)),
    );

    let categories = (0..AppCategory::ALL.len())
        .map(|index| {
            Rect::new(
                sidebar.origin.x + 12,
                sidebar.origin.y + 16 + index as i32 * (CATEGORY_HEIGHT + CATEGORY_GAP),
                (sidebar.size.width - 24).max(0),
                CATEGORY_HEIGHT,
            )
        })
        .collect();

    let columns = ((content.size.width - 32).max(0) / (CARD_WIDTH + CARD_GAP)).max(1) as usize;
    let columns = columns.min(GRID_COLUMNS);
    let cards = (0..visible_count)
        .map(|index| {
            let column = index % columns;
            let row = index / columns;
            Rect::new(
                content.origin.x + 16 + column as i32 * (CARD_WIDTH + CARD_GAP),
                content.origin.y + 16 + row as i32 * (CARD_HEIGHT + CARD_GAP),
                CARD_WIDTH.min(content.size.width.max(0)),
                CARD_HEIGHT.min(content.size.height.max(0)),
            )
        })
        .collect();

    AppStoreLayout {
        window,
        sidebar,
        toolbar,
        search,
        content,
        categories,
        cards,
    }
}

/// The pure application-gallery state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppStore {
    /// All entries supplied by the backend, including entries filtered out of
    /// the current view.
    pub apps: Vec<FlatpakApp>,
    /// Current case-insensitive search text.
    pub query: String,
    /// Current category filter.
    pub category: AppCategory,
    /// Most recent backend state.
    pub state: CatalogState,
    /// Index in the current filtered view, not in `apps`.
    pub selected: Option<usize>,
}

impl Default for AppStore {
    fn default() -> Self {
        Self::new()
    }
}

impl AppStore {
    /// Start with a loading state so the renderer can show a skeleton before
    /// the first backend snapshot arrives.
    pub fn new() -> Self {
        Self {
            apps: Vec::new(),
            query: String::new(),
            category: AppCategory::All,
            state: CatalogState::Loading,
            selected: None,
        }
    }

    /// Start from a local/live list and derive its ready/empty state.
    pub fn from_apps(apps: Vec<FlatpakApp>) -> Self {
        let mut store = Self::new();
        store.set_apps(apps);
        store
    }

    /// Replace the catalogue with live data.
    pub fn set_apps(&mut self, apps: Vec<FlatpakApp>) {
        self.apps = apps;
        self.state = if self.apps.is_empty() {
            CatalogState::Empty
        } else {
            CatalogState::Ready
        };
        self.retain_valid_selection();
    }

    /// Replace the catalogue with cached data and explicitly mark it offline.
    pub fn set_cached_apps(&mut self, apps: Vec<FlatpakApp>) {
        self.apps = apps;
        self.state = CatalogState::Offline;
        self.retain_valid_selection();
    }

    /// Mark a refresh as in progress without throwing away the last visible
    /// list. This avoids a visual flash when refreshing a stale cache.
    pub fn set_loading(&mut self) {
        self.state = CatalogState::Loading;
    }

    /// Record a non-offline backend error while retaining existing entries.
    pub fn set_error(&mut self, message: impl Into<String>) {
        self.state = CatalogState::Error(message.into());
        self.retain_valid_selection();
    }

    /// Mark the view offline while retaining existing entries.
    pub fn set_offline(&mut self) {
        self.state = CatalogState::Offline;
        self.retain_valid_selection();
    }

    /// Update the search text and clear a selection that no longer exists.
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.retain_valid_selection();
    }

    /// Set the category filter and clear a selection that no longer exists.
    pub fn set_category(&mut self, category: AppCategory) {
        self.category = category;
        self.retain_valid_selection();
    }

    /// Set a category by the visual button index.
    pub fn set_category_index(&mut self, index: usize) -> bool {
        let Some(category) = AppCategory::ALL.get(index).copied() else {
            return false;
        };
        self.set_category(category);
        true
    }

    /// Apps in the current query/category view.
    pub fn filtered(&self) -> Vec<&FlatpakApp> {
        self.apps
            .iter()
            .filter(|app| app.in_category(self.category) && app.matches_query(&self.query))
            .collect()
    }

    /// Original-list indices for the current query/category view.
    pub fn filtered_indices(&self) -> Vec<usize> {
        self.apps
            .iter()
            .enumerate()
            .filter(|(_, app)| app.in_category(self.category) && app.matches_query(&self.query))
            .map(|(index, _)| index)
            .collect()
    }

    /// Whether the current search/category view has no cards.
    pub fn view_is_empty(&self) -> bool {
        self.filtered_indices().is_empty()
    }

    /// Whether an otherwise loaded catalogue has no search hits.
    pub fn shows_no_results(&self) -> bool {
        !self.apps.is_empty() && self.view_is_empty()
    }

    /// Select a card in the current filtered view.
    pub fn select(&mut self, index: usize) -> bool {
        if index >= self.filtered_indices().len() {
            return false;
        }
        self.selected = Some(index);
        true
    }

    /// Select the card hit by a point, returning its app ID for action routing.
    pub fn select_at(&mut self, work_area: Rect, point: Point) -> Option<String> {
        let layout = self.layout(work_area);
        let AppStoreHit::Card(index) = layout.hit_test(point) else {
            return None;
        };
        if !self.select(index) {
            return None;
        }
        self.selected_app().map(|app| app.app_id().to_owned())
    }

    /// Clear the current card selection.
    pub fn clear_selection(&mut self) {
        self.selected = None;
    }

    /// The selected app in the current filtered view.
    pub fn selected_app(&self) -> Option<&FlatpakApp> {
        let index = self.selected?;
        let original = *self.filtered_indices().get(index)?;
        self.apps.get(original)
    }

    /// The selected app ID, useful when dispatching install/update actions.
    pub fn selected_app_id(&self) -> Option<&str> {
        self.selected_app().map(FlatpakApp::app_id)
    }

    /// Compute renderer geometry for the current view.
    pub fn layout(&self, work_area: Rect) -> AppStoreLayout {
        layout(window_rect(work_area), self.filtered_indices().len())
    }

    /// Hit-test this store's current view.
    pub fn hit_test(&self, work_area: Rect, point: Point) -> AppStoreHit {
        self.layout(work_area).hit_test(point)
    }

    fn retain_valid_selection(&mut self) {
        if self
            .selected
            .is_some_and(|index| index >= self.filtered_indices().len())
        {
            self.selected = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK_AREA: Rect = Rect::new(0, 0, 1440, 900);

    fn app(id: &str, name: &str, categories: &[&str]) -> FlatpakApp {
        let mut app = FlatpakApp::new(id, name, format!("{name} summary"));
        app.metadata.categories = categories.iter().map(|value| (*value).to_owned()).collect();
        app
    }

    #[test]
    fn category_mapping_keeps_unknown_metadata_honest() {
        assert_eq!(
            AppCategory::from_flatpak_category("Game"),
            Some(AppCategory::Games)
        );
        assert_eq!(
            AppCategory::from_flatpak_category("AudioVideo"),
            Some(AppCategory::AudioVideo)
        );
        assert_eq!(AppCategory::from_flatpak_category("MadeUp"), None);
        assert_eq!(AppCategory::from_name("MadeUp"), AppCategory::Other);
    }

    #[test]
    fn category_and_search_filters_are_composable() {
        let mut store = AppStore::from_apps(vec![
            app("org.gnome.Calculator", "Calculator", &["Utility"]),
            app("org.gnome.Music", "Music", &["AudioVideo"]),
            app("org.gnome.Builder", "Builder", &["Development"]),
        ]);

        store.set_category(AppCategory::AudioVideo);
        assert_eq!(
            store.filtered().iter().map(|app| app.name()).collect::<Vec<_>>(),
            ["Music"]
        );

        store.set_category(AppCategory::All);
        store.set_query("CALC");
        assert_eq!(
            store
                .filtered()
                .iter()
                .map(|app| app.app_id())
                .collect::<Vec<_>>(),
            ["org.gnome.Calculator"]
        );
    }

    #[test]
    fn loading_empty_error_and_offline_states_are_distinct() {
        let mut store = AppStore::new();
        assert!(store.state.is_loading());

        store.set_apps(Vec::new());
        assert_eq!(store.state, CatalogState::Empty);

        store.set_error("permission denied");
        assert_eq!(store.state.message(), Some("permission denied"));
        assert!(store.state.is_error());

        store.set_offline();
        assert!(store.state.is_offline());
    }

    #[test]
    fn selecting_and_hitting_cards_uses_filtered_indices() {
        let mut store = AppStore::from_apps(vec![
            app("org.gnome.Calculator", "Calculator", &["Utility"]),
            app("org.gnome.Music", "Music", &["AudioVideo"]),
        ]);
        store.set_query("music");
        assert!(store.select(0));
        assert_eq!(store.selected_app_id(), Some("org.gnome.Music"));

        let card = store.layout(WORK_AREA).cards[0];
        let point = Point {
            x: card.origin.x + card.size.width / 2,
            y: card.origin.y + card.size.height / 2,
        };
        assert_eq!(store.hit_test(WORK_AREA, point), AppStoreHit::Card(0));
        assert_eq!(
            store.select_at(WORK_AREA, point).as_deref(),
            Some("org.gnome.Music")
        );
    }

    #[test]
    fn changing_filter_drops_invalid_selection() {
        let mut store = AppStore::from_apps(vec![app("org.gnome.Calculator", "Calculator", &["Utility"])]);
        assert!(store.select(0));
        store.set_query("missing");
        assert_eq!(store.selected, None);
        assert!(store.shows_no_results());
    }

    #[test]
    fn layout_is_clamped_and_hit_targets_do_not_overlap() {
        let tiny = window_rect(Rect::new(0, 0, 400, 260));
        assert!(tiny.size.width <= 400);
        assert!(tiny.size.height <= 260);

        let layout = layout(Rect::new(0, 0, 980, 650), 8);
        assert_eq!(layout.cards.len(), 8);
        for (index, first) in layout.cards.iter().enumerate() {
            for second in layout.cards.iter().skip(index + 1) {
                let separated = first.right() <= second.origin.x
                    || second.right() <= first.origin.x
                    || first.bottom() <= second.origin.y
                    || second.bottom() <= first.origin.y;
                assert!(separated, "{first:?} overlaps {second:?}");
            }
        }

        let search_point = Point {
            x: layout.search.origin.x + 2,
            y: layout.search.origin.y + 2,
        };
        assert_eq!(layout.hit_test(search_point), AppStoreHit::Search);
        assert_eq!(layout.hit_test(Point { x: -1, y: -1 }), AppStoreHit::None);
    }

    #[test]
    fn app_actions_reflect_installation_state() {
        let mut app = FlatpakApp::new("org.example.App", "App", "A test app");
        assert_eq!(app.primary_action(), AppAction::Install);
        app.installed = true;
        assert_eq!(app.primary_action(), AppAction::Open);
        app.update_available = true;
        assert_eq!(app.primary_action(), AppAction::Update);
    }
}
