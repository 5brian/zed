#![allow(rustdoc::private_intra_doc_links)]
//! This is the place where everything editor-related is stored (data-wise) and displayed (ui-wise).
//! The main point of interest in this crate is [`Editor`] type, which is used in every other Zed part as a user input element.
//! It comes in different flavors: single line, multiline and a fixed height one.
//!
//! Editor contains of multiple large submodules:
//! * [`element`] — the place where all rendering happens
//! * [`display_map`] - chunks up text in the editor into the logical blocks, establishes coordinates and mapping between each of them.
//!   Contains all metadata related to text transformations (folds, fake inlay text insertions, soft wraps, tab markup, etc.).
//! * [`inlay_hint_cache`] - is a storage of inlay hints out of LSP requests, responsible for querying LSP and updating `display_map`'s state accordingly.
//!
//! All other submodules and structs are mostly concerned with holding editor data about the way it displays current buffer region(s).
//!
//! If you're looking to improve Vim mode, you should check out Vim crate that wraps Editor and overrides its behavior.
pub mod actions;
mod blame_entry_tooltip;
mod blink_manager;
mod clangd_ext;
mod code_context_menus;
pub mod display_map;
mod editor_settings;
mod editor_settings_controls;
mod element;
mod git;
mod highlight_matching_bracket;
mod hover_links;
mod hover_popover;
mod indent_guides;
mod inlay_hint_cache;
pub mod items;
mod linked_editing_ranges;
mod lsp_ext;
mod mouse_context_menu;
pub mod movement;
mod persistence;
mod proposed_changes_editor;
mod rust_analyzer_ext;
pub mod scroll;
mod selections_collection;
pub mod tasks;

#[cfg(test)]
mod editor_tests;
#[cfg(test)]
mod inline_completion_tests;
mod signature_help;
#[cfg(any(test, feature = "test-support"))]
pub mod test;

pub(crate) use actions::*;
pub use actions::{OpenExcerpts, OpenExcerptsSplit};
use aho_corasick::AhoCorasick;
use anyhow::{anyhow, Context as _, Result};
use blink_manager::BlinkManager;
use client::{Collaborator, ParticipantIndex};
use clock::ReplicaId;
use collections::{BTreeMap, HashMap, HashSet, VecDeque};
use convert_case::{Case, Casing};
use display_map::*;
pub use display_map::{DisplayPoint, FoldPlaceholder};
pub use editor_settings::{
    CurrentLineHighlight, EditorSettings, ScrollBeyondLastLine, SearchSettings, ShowScrollbar,
};
pub use editor_settings_controls::*;
pub use element::{
    CursorLayout, EditorElement, HighlightedRange, HighlightedRangeLine, PointForPosition,
};
use element::{LineWithInvisibles, PositionMap};
use futures::{future, FutureExt};
use fuzzy::StringMatchCandidate;

use code_context_menus::{
    AvailableCodeAction, CodeActionContents, CodeActionsItem, CodeActionsMenu, CodeContextMenu,
    CompletionsMenu, ContextMenuOrigin,
};
use diff::DiffHunkStatus;
use git::blame::GitBlame;
use gpui::{
    div, impl_actions, linear_color_stop, linear_gradient, point, prelude::*, pulsating_between,
    px, relative, size, Action, Animation, AnimationExt, AnyElement, App, AsyncWindowContext,
    AvailableSpace, Bounds, ClipboardEntry, ClipboardItem, Context, DispatchPhase, ElementId,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, FocusOutEvent, Focusable, FontId,
    FontWeight, Global, HighlightStyle, Hsla, InteractiveText, KeyContext, Modifiers, MouseButton,
    MouseDownEvent, PaintQuad, ParentElement, Pixels, Render, SharedString, Size, Styled,
    StyledText, Subscription, Task, TextRun, TextStyle, TextStyleRefinement, UTF16Selection,
    UnderlineStyle, UniformListScrollHandle, WeakEntity, WeakFocusHandle, Window,
};
use highlight_matching_bracket::refresh_matching_bracket_highlights;
use hover_popover::{hide_hover, HoverState};
use indent_guides::ActiveIndentGuidesState;
use inlay_hint_cache::{InlayHintCache, InlaySplice, InvalidationStrategy};
pub use inline_completion::Direction;
use inline_completion::{InlineCompletionProvider, InlineCompletionProviderHandle};
pub use items::MAX_TAB_TITLE_LEN;
use itertools::Itertools;
use language::{
    language_settings::{self, all_language_settings, language_settings, InlayHintSettings},
    markdown, point_from_lsp, AutoindentMode, BracketPair, Buffer, Capability, CharKind, CodeLabel,
    CompletionDocumentation, CursorShape, Diagnostic, EditPreview, HighlightedText, IndentKind,
    IndentSize, InlineCompletionPreviewMode, Language, OffsetRangeExt, Point, Selection,
    SelectionGoal, TextObject, TransactionId, TreeSitterOptions,
};
use language::{point_to_lsp, BufferRow, CharClassifier, Runnable, RunnableRange};
use linked_editing_ranges::refresh_linked_ranges;
use mouse_context_menu::MouseContextMenu;
pub use proposed_changes_editor::{
    ProposedChangeLocation, ProposedChangesEditor, ProposedChangesEditorToolbar,
};
use similar::{ChangeTag, TextDiff};
use std::iter::Peekable;
use task::{ResolvedTask, TaskTemplate, TaskVariables};

use hover_links::{find_file, HoverLink, HoveredLinkState, InlayHighlight};
pub use lsp::CompletionContext;
use lsp::{
    CompletionItemKind, CompletionTriggerKind, DiagnosticSeverity, InsertTextFormat,
    LanguageServerId, LanguageServerName,
};

use language::BufferSnapshot;
use movement::TextLayoutDetails;
pub use multi_buffer::{
    Anchor, AnchorRangeExt, ExcerptId, ExcerptRange, MultiBuffer, MultiBufferSnapshot, RowInfo,
    ToOffset, ToPoint,
};
use multi_buffer::{
    ExcerptInfo, ExpandExcerptDirection, MultiBufferDiffHunk, MultiBufferPoint, MultiBufferRow,
    ToOffsetUtf16,
};
use project::{
    lsp_store::{FormatTrigger, LspFormatTarget, OpenLspBufferHandle},
    project_settings::{GitGutterSetting, ProjectSettings},
    CodeAction, Completion, CompletionIntent, DocumentHighlight, InlayHint, Location, LocationLink,
    LspStore, PrepareRenameResponse, Project, ProjectItem, ProjectTransaction, TaskSourceKind,
};
use rand::prelude::*;
use rpc::{proto::*, ErrorExt};
use scroll::{Autoscroll, OngoingScroll, ScrollAnchor, ScrollManager, ScrollbarAutoHide};
use selections_collection::{
    resolve_selections, MutableSelectionsCollection, SelectionsCollection,
};
use serde::{Deserialize, Serialize};
use settings::{update_settings_file, Settings, SettingsLocation, SettingsStore};
use smallvec::SmallVec;
use snippet::Snippet;
use std::{
    any::TypeId,
    borrow::Cow,
    cell::RefCell,
    cmp::{self, Ordering, Reverse},
    mem,
    num::NonZeroU32,
    ops::{ControlFlow, Deref, DerefMut, Not as _, Range, RangeInclusive},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
pub use sum_tree::Bias;
use sum_tree::TreeMap;
use text::{BufferId, OffsetUtf16, Rope};
use theme::{ActiveTheme, PlayerColor, StatusColors, SyntaxTheme, ThemeColors, ThemeSettings};
use ui::{
    h_flex, prelude::*, ButtonSize, ButtonStyle, Disclosure, IconButton, IconName, IconSize,
    Tooltip,
};
use util::{defer, maybe, post_inc, RangeExt, ResultExt, TakeUntilExt, TryFutureExt};
use workspace::item::{ItemHandle, PreviewTabsSettings};
use workspace::notifications::{DetachAndPromptErr, NotificationId, NotifyTaskExt};
use workspace::{
    searchable::SearchEvent, ItemNavHistory, SplitDirection, ViewId, Workspace, WorkspaceId,
};
use workspace::{Item as WorkspaceItem, OpenInTerminal, OpenTerminal, TabBarSettings, Toast};

use crate::hover_links::{find_url, find_url_from_range};
use crate::signature_help::{SignatureHelpHiddenBy, SignatureHelpState};

pub const FILE_HEADER_HEIGHT: u32 = 2;
pub const MULTI_BUFFER_EXCERPT_HEADER_HEIGHT: u32 = 1;
pub const MULTI_BUFFER_EXCERPT_FOOTER_HEIGHT: u32 = 1;
pub const DEFAULT_MULTIBUFFER_CONTEXT: u32 = 2;
const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(500);
const MAX_LINE_LEN: usize = 1024;
const MIN_NAVIGATION_HISTORY_ROW_DELTA: i64 = 10;
const MAX_SELECTION_HISTORY_LEN: usize = 1024;
pub(crate) const CURSORS_VISIBLE_FOR: Duration = Duration::from_millis(2000);
#[doc(hidden)]
pub const CODE_ACTIONS_DEBOUNCE_TIMEOUT: Duration = Duration::from_millis(250);

pub(crate) const FORMAT_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const SCROLL_CENTER_TOP_BOTTOM_DEBOUNCE_TIMEOUT: Duration = Duration::from_secs(1);

pub fn render_parsed_markdown(
    element_id: impl Into<ElementId>,
    parsed: &language::ParsedMarkdown,
    editor_style: &EditorStyle,
    workspace: Option<WeakEntity<Workspace>>,
    cx: &mut App,
) -> InteractiveText {
    let code_span_background_color = cx
        .theme()
        .colors()
        .editor_document_highlight_read_background;

    let highlights = gpui::combine_highlights(
        parsed.highlights.iter().filter_map(|(range, highlight)| {
            let highlight = highlight.to_highlight_style(&editor_style.syntax)?;
            Some((range.clone(), highlight))
        }),
        parsed
            .regions
            .iter()
            .zip(&parsed.region_ranges)
            .filter_map(|(region, range)| {
                if region.code {
                    Some((
                        range.clone(),
                        HighlightStyle {
                            background_color: Some(code_span_background_color),
                            ..Default::default()
                        },
                    ))
                } else {
                    None
                }
            }),
    );

    let mut links = Vec::new();
    let mut link_ranges = Vec::new();
    for (range, region) in parsed.region_ranges.iter().zip(&parsed.regions) {
        if let Some(link) = region.link.clone() {
            links.push(link);
            link_ranges.push(range.clone());
        }
    }

    InteractiveText::new(
        element_id,
        StyledText::new(parsed.text.clone()).with_highlights(&editor_style.text, highlights),
    )
    .on_click(
        link_ranges,
        move |clicked_range_ix, window, cx| match &links[clicked_range_ix] {
            markdown::Link::Web { url } => cx.open_url(url),
            markdown::Link::Path { path } => {
                if let Some(workspace) = &workspace {
                    _ = workspace.update(cx, |workspace, cx| {
                        workspace
                            .open_abs_path(path.clone(), false, window, cx)
                            .detach();
                    });
                }
            }
        },
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InlayId {
    InlineCompletion(usize),
    Hint(usize),
}

impl InlayId {
    fn id(&self) -> usize {
        match self {
            Self::InlineCompletion(id) => *id,
            Self::Hint(id) => *id,
        }
    }
}

enum DocumentHighlightRead {}
enum DocumentHighlightWrite {}
enum InputComposition {}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Navigated {
    Yes,
    No,
}

impl Navigated {
    pub fn from_bool(yes: bool) -> Navigated {
        if yes {
            Navigated::Yes
        } else {
            Navigated::No
        }
    }
}

pub fn init_settings(cx: &mut App) {
    EditorSettings::register(cx);
}

pub fn init(cx: &mut App) {
    init_settings(cx);

    workspace::register_project_item::<Editor>(cx);
    workspace::FollowableViewRegistry::register::<Editor>(cx);
    workspace::register_serializable_item::<Editor>(cx);

    cx.observe_new(
        |workspace: &mut Workspace, _: Option<&mut Window>, _cx: &mut Context<Workspace>| {
            workspace.register_action(Editor::new_file);
            workspace.register_action(Editor::new_file_vertical);
            workspace.register_action(Editor::new_file_horizontal);
            workspace.register_action(Editor::cancel_language_server_work);
        },
    )
    .detach();

    cx.on_action(move |_: &workspace::NewFile, cx| {
        let app_state = workspace::AppState::global(cx);
        if let Some(app_state) = app_state.upgrade() {
            workspace::open_new(
                Default::default(),
                app_state,
                cx,
                |workspace, window, cx| {
                    Editor::new_file(workspace, &Default::default(), window, cx)
                },
            )
            .detach();
        }
    });
    cx.on_action(move |_: &workspace::NewWindow, cx| {
        let app_state = workspace::AppState::global(cx);
        if let Some(app_state) = app_state.upgrade() {
            workspace::open_new(
                Default::default(),
                app_state,
                cx,
                |workspace, window, cx| {
                    cx.activate(true);
                    Editor::new_file(workspace, &Default::default(), window, cx)
                },
            )
            .detach();
        }
    });
}

pub struct SearchWithinRange;

trait InvalidationRegion {
    fn ranges(&self) -> &[Range<Anchor>];
}

#[derive(Clone, Debug, PartialEq)]
pub enum SelectPhase {
    Begin {
        position: DisplayPoint,
        add: bool,
        click_count: usize,
    },
    BeginColumnar {
        position: DisplayPoint,
        reset: bool,
        goal_column: u32,
    },
    Extend {
        position: DisplayPoint,
        click_count: usize,
    },
    Update {
        position: DisplayPoint,
        goal_column: u32,
        scroll_delta: gpui::Point<f32>,
    },
    End,
}

#[derive(Clone, Debug)]
pub enum SelectMode {
    Character,
    Word(Range<Anchor>),
    Line(Range<Anchor>),
    All,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum EditorMode {
    SingleLine { auto_width: bool },
    AutoHeight { max_lines: usize },
    Full,
}

#[derive(Copy, Clone, Debug)]
pub enum SoftWrap {
    /// Prefer not to wrap at all.
    ///
    /// Note: this is currently internal, as actually limited by [`crate::MAX_LINE_LEN`] until it wraps.
    /// The mode is used inside git diff hunks, where it's seems currently more useful to not wrap as much as possible.
    GitDiff,
    /// Prefer a single line generally, unless an overly long line is encountered.
    None,
    /// Soft wrap lines that exceed the editor width.
    EditorWidth,
    /// Soft wrap lines at the preferred line length.
    Column(u32),
    /// Soft wrap line at the preferred line length or the editor width (whichever is smaller).
    Bounded(u32),
}

#[derive(Clone)]
pub struct EditorStyle {
    pub background: Hsla,
    pub local_player: PlayerColor,
    pub text: TextStyle,
    pub scrollbar_width: Pixels,
    pub syntax: Arc<SyntaxTheme>,
    pub status: StatusColors,
    pub inlay_hints_style: HighlightStyle,
    pub inline_completion_styles: InlineCompletionStyles,
    pub unnecessary_code_fade: f32,
}

impl Default for EditorStyle {
    fn default() -> Self {
        Self {
            background: Hsla::default(),
            local_player: PlayerColor::default(),
            text: TextStyle::default(),
            scrollbar_width: Pixels::default(),
            syntax: Default::default(),
            // HACK: Status colors don't have a real default.
            // We should look into removing the status colors from the editor
            // style and retrieve them directly from the theme.
            status: StatusColors::dark(),
            inlay_hints_style: HighlightStyle::default(),
            inline_completion_styles: InlineCompletionStyles {
                insertion: HighlightStyle::default(),
                whitespace: HighlightStyle::default(),
            },
            unnecessary_code_fade: Default::default(),
        }
    }
}

pub fn make_inlay_hints_style(cx: &mut App) -> HighlightStyle {
    let show_background = language_settings::language_settings(None, None, cx)
        .inlay_hints
        .show_background;

    HighlightStyle {
        color: Some(cx.theme().status().hint),
        background_color: show_background.then(|| cx.theme().status().hint_background),
        ..HighlightStyle::default()
    }
}

pub fn make_suggestion_styles(cx: &mut App) -> InlineCompletionStyles {
    InlineCompletionStyles {
        insertion: HighlightStyle {
            color: Some(cx.theme().status().predictive),
            ..HighlightStyle::default()
        },
        whitespace: HighlightStyle {
            background_color: Some(cx.theme().status().created_background),
            ..HighlightStyle::default()
        },
    }
}

type CompletionId = usize;

pub(crate) enum EditDisplayMode {
    TabAccept,
    DiffPopover,
    Inline,
}

enum InlineCompletion {
    Edit {
        edits: Vec<(Range<Anchor>, String)>,
        edit_preview: Option<EditPreview>,
        display_mode: EditDisplayMode,
        snapshot: BufferSnapshot,
    },
    Move {
        target: Anchor,
        range_around_target: Range<text::Anchor>,
        snapshot: BufferSnapshot,
    },
}

struct InlineCompletionState {
    inlay_ids: Vec<InlayId>,
    completion: InlineCompletion,
    invalidation_range: Range<Anchor>,
}

enum InlineCompletionHighlight {}

pub enum MenuInlineCompletionsPolicy {
    Never,
    ByProvider,
}

#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Debug, Default)]
struct EditorActionId(usize);

impl EditorActionId {
    pub fn post_inc(&mut self) -> Self {
        let answer = self.0;

        *self = Self(answer + 1);

        Self(answer)
    }
}

// type GetFieldEditorTheme = dyn Fn(&theme::Theme) -> theme::FieldEditor;
// type OverrideTextStyle = dyn Fn(&EditorStyle) -> Option<HighlightStyle>;

type BackgroundHighlight = (fn(&ThemeColors) -> Hsla, Arc<[Range<Anchor>]>);
type GutterHighlight = (fn(&App) -> Hsla, Arc<[Range<Anchor>]>);

#[derive(Default)]
struct ScrollbarMarkerState {
    scrollbar_size: Size<Pixels>,
    dirty: bool,
    markers: Arc<[PaintQuad]>,
    pending_refresh: Option<Task<Result<()>>>,
}

impl ScrollbarMarkerState {
    fn should_refresh(&self, scrollbar_size: Size<Pixels>) -> bool {
        self.pending_refresh.is_none() && (self.scrollbar_size != scrollbar_size || self.dirty)
    }
}

#[derive(Clone, Debug)]
struct RunnableTasks {
    templates: Vec<(TaskSourceKind, TaskTemplate)>,
    offset: MultiBufferOffset,
    // We need the column at which the task context evaluation should take place (when we're spawning it via gutter).
    column: u32,
    // Values of all named captures, including those starting with '_'
    extra_variables: HashMap<String, String>,
    // Full range of the tagged region. We use it to determine which `extra_variables` to grab for context resolution in e.g. a modal.
    context_range: Range<BufferOffset>,
}

impl RunnableTasks {
    fn resolve<'a>(
        &'a self,
        cx: &'a task::TaskContext,
    ) -> impl Iterator<Item = (TaskSourceKind, ResolvedTask)> + 'a {
        self.templates.iter().filter_map(|(kind, template)| {
            template
                .resolve_task(&kind.to_id_base(), cx)
                .map(|task| (kind.clone(), task))
        })
    }
}

#[derive(Clone)]
struct ResolvedTasks {
    templates: SmallVec<[(TaskSourceKind, ResolvedTask); 1]>,
    position: Anchor,
}
#[derive(Copy, Clone, Debug)]
struct MultiBufferOffset(usize);
#[derive(Copy, Clone, Debug, PartialEq, PartialOrd)]
struct BufferOffset(usize);

// Addons allow storing per-editor state in other crates (e.g. Vim)
pub trait Addon: 'static {
    fn extend_key_context(&self, _: &mut KeyContext, _: &App) {}

    fn render_buffer_header_controls(
        &self,
        _: &ExcerptInfo,
        _: &Window,
        _: &App,
    ) -> Option<AnyElement> {
        None
    }

    fn to_any(&self) -> &dyn std::any::Any;
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum IsVimMode {
    Yes,
    No,
}

/// Zed's primary implementation of text input, allowing users to edit a [`MultiBuffer`].
///
/// See the [module level documentation](self) for more information.
pub struct Editor {
    focus_handle: FocusHandle,
    last_focused_descendant: Option<WeakFocusHandle>,
    /// The text buffer being edited
    buffer: Entity<MultiBuffer>,
    /// Map of how text in the buffer should be displayed.
    /// Handles soft wraps, folds, fake inlay text insertions, etc.
    pub display_map: Entity<DisplayMap>,
    pub selections: SelectionsCollection,
    pub scroll_manager: ScrollManager,
    /// When inline assist editors are linked, they all render cursors because
    /// typing enters text into each of them, even the ones that aren't focused.
    pub(crate) show_cursor_when_unfocused: bool,
    columnar_selection_tail: Option<Anchor>,
    add_selections_state: Option<AddSelectionsState>,
    select_next_state: Option<SelectNextState>,
    select_prev_state: Option<SelectNextState>,
    selection_history: SelectionHistory,
    autoclose_regions: Vec<AutocloseRegion>,
    snippet_stack: InvalidationStack<SnippetState>,
    select_larger_syntax_node_stack: Vec<Box<[Selection<usize>]>>,
    ime_transaction: Option<TransactionId>,
    active_diagnostics: Option<ActiveDiagnosticGroup>,
    soft_wrap_mode_override: Option<language_settings::SoftWrap>,

    // TODO: make this a access method
    pub project: Option<Entity<Project>>,
    semantics_provider: Option<Rc<dyn SemanticsProvider>>,
    completion_provider: Option<Box<dyn CompletionProvider>>,
    collaboration_hub: Option<Box<dyn CollaborationHub>>,
    blink_manager: Entity<BlinkManager>,
    show_cursor_names: bool,
    hovered_cursors: HashMap<HoveredCursor, Task<()>>,
    pub show_local_selections: bool,
    mode: EditorMode,
    show_breadcrumbs: bool,
    show_gutter: bool,
    show_scrollbars: bool,
    show_line_numbers: Option<bool>,
    use_relative_line_numbers: Option<bool>,
    show_git_diff_gutter: Option<bool>,
    show_code_actions: Option<bool>,
    show_runnables: Option<bool>,
    show_wrap_guides: Option<bool>,
    show_indent_guides: Option<bool>,
    placeholder_text: Option<Arc<str>>,
    highlight_order: usize,
    highlighted_rows: HashMap<TypeId, Vec<RowHighlight>>,
    background_highlights: TreeMap<TypeId, BackgroundHighlight>,
    gutter_highlights: TreeMap<TypeId, GutterHighlight>,
    scrollbar_marker_state: ScrollbarMarkerState,
    active_indent_guides_state: ActiveIndentGuidesState,
    nav_history: Option<ItemNavHistory>,
    context_menu: RefCell<Option<CodeContextMenu>>,
    mouse_context_menu: Option<MouseContextMenu>,
    completion_tasks: Vec<(CompletionId, Task<Option<()>>)>,
    signature_help_state: SignatureHelpState,
    auto_signature_help: Option<bool>,
    find_all_references_task_sources: Vec<Anchor>,
    next_completion_id: CompletionId,
    available_code_actions: Option<(Location, Rc<[AvailableCodeAction]>)>,
    code_actions_task: Option<Task<Result<()>>>,
    document_highlights_task: Option<Task<()>>,
    linked_editing_range_task: Option<Task<Option<()>>>,
    linked_edit_ranges: linked_editing_ranges::LinkedEditingRanges,
    pending_rename: Option<RenameState>,
    searchable: bool,
    cursor_shape: CursorShape,
    current_line_highlight: Option<CurrentLineHighlight>,
    collapse_matches: bool,
    autoindent_mode: Option<AutoindentMode>,
    workspace: Option<(WeakEntity<Workspace>, Option<WorkspaceId>)>,
    input_enabled: bool,
    use_modal_editing: bool,
    read_only: bool,
    leader_peer_id: Option<PeerId>,
    remote_id: Option<ViewId>,
    hover_state: HoverState,
    pending_mouse_down: Option<Rc<RefCell<Option<MouseDownEvent>>>>,
    gutter_hovered: bool,
    hovered_link_state: Option<HoveredLinkState>,
    inline_completion_provider: Option<RegisteredInlineCompletionProvider>,
    code_action_providers: Vec<Rc<dyn CodeActionProvider>>,
    active_inline_completion: Option<InlineCompletionState>,
    /// Used to prevent flickering as the user types while the menu is open
    stale_inline_completion_in_menu: Option<InlineCompletionState>,
    // enable_inline_completions is a switch that Vim can use to disable
    // edit predictions based on its mode.
    show_inline_completions: bool,
    show_inline_completions_override: Option<bool>,
    menu_inline_completions_policy: MenuInlineCompletionsPolicy,
    previewing_inline_completion: bool,
    inlay_hint_cache: InlayHintCache,
    next_inlay_id: usize,
    _subscriptions: Vec<Subscription>,
    pixel_position_of_newest_cursor: Option<gpui::Point<Pixels>>,
    gutter_dimensions: GutterDimensions,
    style: Option<EditorStyle>,
    text_style_refinement: Option<TextStyleRefinement>,
    next_editor_action_id: EditorActionId,
    editor_actions:
        Rc<RefCell<BTreeMap<EditorActionId, Box<dyn Fn(&mut Window, &mut Context<Self>)>>>>,
    use_autoclose: bool,
    use_auto_surround: bool,
    auto_replace_emoji_shortcode: bool,
    show_git_blame_gutter: bool,
    show_git_blame_inline: bool,
    show_git_blame_inline_delay_task: Option<Task<()>>,
    git_blame_inline_enabled: bool,
    serialize_dirty_buffers: bool,
    show_selection_menu: Option<bool>,
    blame: Option<Entity<GitBlame>>,
    blame_subscription: Option<Subscription>,
    custom_context_menu: Option<
        Box<
            dyn 'static
                + Fn(
                    &mut Self,
                    DisplayPoint,
                    &mut Window,
                    &mut Context<Self>,
                ) -> Option<Entity<ui::ContextMenu>>,
        >,
    >,
    last_bounds: Option<Bounds<Pixels>>,
    last_position_map: Option<Rc<PositionMap>>,
    expect_bounds_change: Option<Bounds<Pixels>>,
    tasks: BTreeMap<(BufferId, BufferRow), RunnableTasks>,
    tasks_update_task: Option<Task<()>>,
    in_project_search: bool,
    previous_search_ranges: Option<Arc<[Range<Anchor>]>>,
    breadcrumb_header: Option<String>,
    focused_block: Option<FocusedBlock>,
    next_scroll_position: NextScrollCursorCenterTopBottom,
    addons: HashMap<TypeId, Box<dyn Addon>>,
    registered_buffers: HashMap<BufferId, OpenLspBufferHandle>,
    selection_mark_mode: bool,
    toggle_fold_multiple_buffers: Task<()>,
    _scroll_cursor_center_top_bottom_task: Task<()>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
enum NextScrollCursorCenterTopBottom {
    #[default]
    Center,
    Top,
    Bottom,
}

impl NextScrollCursorCenterTopBottom {
    fn next(&self) -> Self {
        match self {
            Self::Center => Self::Top,
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Center,
        }
    }
}

#[derive(Clone)]
pub struct EditorSnapshot {
    pub mode: EditorMode,
    show_gutter: bool,
    show_line_numbers: Option<bool>,
    show_git_diff_gutter: Option<bool>,
    show_code_actions: Option<bool>,
    show_runnables: Option<bool>,
    git_blame_gutter_max_author_length: Option<usize>,
    pub display_snapshot: DisplaySnapshot,
    pub placeholder_text: Option<Arc<str>>,
    is_focused: bool,
    scroll_anchor: ScrollAnchor,
    ongoing_scroll: OngoingScroll,
    current_line_highlight: CurrentLineHighlight,
    gutter_hovered: bool,
}

const GIT_BLAME_MAX_AUTHOR_CHARS_DISPLAYED: usize = 20;

#[derive(Default, Debug, Clone, Copy)]
pub struct GutterDimensions {
    pub left_padding: Pixels,
    pub right_padding: Pixels,
    pub width: Pixels,
    pub margin: Pixels,
    pub git_blame_entries_width: Option<Pixels>,
}

impl GutterDimensions {
    /// The full width of the space taken up by the gutter.
    pub fn full_width(&self) -> Pixels {
        self.margin + self.width
    }

    /// The width of the space reserved for the fold indicators,
    /// use alongside 'justify_end' and `gutter_width` to
    /// right align content with the line numbers
    pub fn fold_area_width(&self) -> Pixels {
        self.margin + self.right_padding
    }
}

#[derive(Debug)]
pub struct RemoteSelection {
    pub replica_id: ReplicaId,
    pub selection: Selection<Anchor>,
    pub cursor_shape: CursorShape,
    pub peer_id: PeerId,
    pub line_mode: bool,
    pub participant_index: Option<ParticipantIndex>,
    pub user_name: Option<SharedString>,
}

#[derive(Clone, Debug)]
struct SelectionHistoryEntry {
    selections: Arc<[Selection<Anchor>]>,
    select_next_state: Option<SelectNextState>,
    select_prev_state: Option<SelectNextState>,
    add_selections_state: Option<AddSelectionsState>,
}

enum SelectionHistoryMode {
    Normal,
    Undoing,
    Redoing,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct HoveredCursor {
    replica_id: u16,
    selection_id: usize,
}

impl Default for SelectionHistoryMode {
    fn default() -> Self {
        Self::Normal
    }
}

#[derive(Default)]
struct SelectionHistory {
    #[allow(clippy::type_complexity)]
    selections_by_transaction:
        HashMap<TransactionId, (Arc<[Selection<Anchor>]>, Option<Arc<[Selection<Anchor>]>>)>,
    mode: SelectionHistoryMode,
    undo_stack: VecDeque<SelectionHistoryEntry>,
    redo_stack: VecDeque<SelectionHistoryEntry>,
}

impl SelectionHistory {
    fn insert_transaction(
        &mut self,
        transaction_id: TransactionId,
        selections: Arc<[Selection<Anchor>]>,
    ) {
        self.selections_by_transaction
            .insert(transaction_id, (selections, None));
    }

    #[allow(clippy::type_complexity)]
    fn transaction(
        &self,
        transaction_id: TransactionId,
    ) -> Option<&(Arc<[Selection<Anchor>]>, Option<Arc<[Selection<Anchor>]>>)> {
        self.selections_by_transaction.get(&transaction_id)
    }

    #[allow(clippy::type_complexity)]
    fn transaction_mut(
        &mut self,
        transaction_id: TransactionId,
    ) -> Option<&mut (Arc<[Selection<Anchor>]>, Option<Arc<[Selection<Anchor>]>>)> {
        self.selections_by_transaction.get_mut(&transaction_id)
    }

    fn push(&mut self, entry: SelectionHistoryEntry) {
        if !entry.selections.is_empty() {
            match self.mode {
                SelectionHistoryMode::Normal => {
                    self.push_undo(entry);
                    self.redo_stack.clear();
                }
                SelectionHistoryMode::Undoing => self.push_redo(entry),
                SelectionHistoryMode::Redoing => self.push_undo(entry),
            }
        }
    }

    fn push_undo(&mut self, entry: SelectionHistoryEntry) {
        if self
            .undo_stack
            .back()
            .map_or(true, |e| e.selections != entry.selections)
        {
            self.undo_stack.push_back(entry);
            if self.undo_stack.len() > MAX_SELECTION_HISTORY_LEN {
                self.undo_stack.pop_front();
            }
        }
    }

    fn push_redo(&mut self, entry: SelectionHistoryEntry) {
        if self
            .redo_stack
            .back()
            .map_or(true, |e| e.selections != entry.selections)
        {
            self.redo_stack.push_back(entry);
            if self.redo_stack.len() > MAX_SELECTION_HISTORY_LEN {
                self.redo_stack.pop_front();
            }
        }
    }
}

struct RowHighlight {
    index: usize,
    range: Range<Anchor>,
    color: Hsla,
    should_autoscroll: bool,
}

#[derive(Clone, Debug)]
struct AddSelectionsState {
    above: bool,
    stack: Vec<usize>,
}

#[derive(Clone)]
struct SelectNextState {
    query: AhoCorasick,
    wordwise: bool,
    done: bool,
}

impl std::fmt::Debug for SelectNextState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct(std::any::type_name::<Self>())
            .field("wordwise", &self.wordwise)
            .field("done", &self.done)
            .finish()
    }
}

#[derive(Debug)]
struct AutocloseRegion {
    selection_id: usize,
    range: Range<Anchor>,
    pair: BracketPair,
}

#[derive(Debug)]
struct SnippetState {
    ranges: Vec<Vec<Range<Anchor>>>,
    active_index: usize,
    choices: Vec<Option<Vec<String>>>,
}

#[doc(hidden)]
pub struct RenameState {
    pub range: Range<Anchor>,
    pub old_name: Arc<str>,
    pub editor: Entity<Editor>,
    block_id: CustomBlockId,
}

struct InvalidationStack<T>(Vec<T>);

struct RegisteredInlineCompletionProvider {
    provider: Arc<dyn InlineCompletionProviderHandle>,
    _subscription: Subscription,
}

#[derive(Debug)]
struct ActiveDiagnosticGroup {
    primary_range: Range<Anchor>,
    primary_message: String,
    group_id: usize,
    blocks: HashMap<CustomBlockId, Diagnostic>,
    is_valid: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ClipboardSelection {
    pub len: usize,
    pub is_entire_line: bool,
    pub first_line_indent: u32,
}

#[derive(Debug)]
pub(crate) struct NavigationData {
    cursor_anchor: Anchor,
    cursor_position: Point,
    scroll_anchor: ScrollAnchor,
    scroll_top_row: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GotoDefinitionKind {
    Symbol,
    Declaration,
    Type,
    Implementation,
}

#[derive(Debug, Clone)]
enum InlayHintRefreshReason {
    Toggle(bool),
    SettingsChange(InlayHintSettings),
    NewLinesShown,
    BufferEdited(HashSet<Arc<Language>>),
    RefreshRequested,
    ExcerptsRemoved(Vec<ExcerptId>),
}

impl InlayHintRefreshReason {
    fn description(&self) -> &'static str {
        match self {
            Self::Toggle(_) => "toggle",
            Self::SettingsChange(_) => "settings change",
            Self::NewLinesShown => "new lines shown",
            Self::BufferEdited(_) => "buffer edited",
            Self::RefreshRequested => "refresh requested",
            Self::ExcerptsRemoved(_) => "excerpts removed",
        }
    }
}

pub enum FormatTarget {
    Buffers,
    Ranges(Vec<Range<MultiBufferPoint>>),
}

pub(crate) struct FocusedBlock {
    id: BlockId,
    focus_handle: WeakFocusHandle,
}

#[derive(Clone)]
enum JumpData {
    MultiBufferRow {
        row: MultiBufferRow,
        line_offset_from_top: u32,
    },
    MultiBufferPoint {
        excerpt_id: ExcerptId,
        position: Point,
        anchor: text::Anchor,
        line_offset_from_top: u32,
    },
}

pub enum MultibufferSelectionMode {
    First,
    All,
}

impl Editor {
    pub fn single_line(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let buffer = cx.new(|cx| Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        Self::new(
            EditorMode::SingleLine { auto_width: false },
            buffer,
            None,
            false,
            window,
            cx,
        )
    }

    pub fn multi_line(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let buffer = cx.new(|cx| Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        Self::new(EditorMode::Full, buffer, None, false, window, cx)
    }

    pub fn auto_width(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let buffer = cx.new(|cx| Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        Self::new(
            EditorMode::SingleLine { auto_width: true },
            buffer,
            None,
            false,
            window,
            cx,
        )
    }

    pub fn auto_height(max_lines: usize, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let buffer = cx.new(|cx| Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        Self::new(
            EditorMode::AutoHeight { max_lines },
            buffer,
            None,
            false,
            window,
            cx,
        )
    }

    pub fn for_buffer(
        buffer: Entity<Buffer>,
        project: Option<Entity<Project>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        Self::new(EditorMode::Full, buffer, project, false, window, cx)
    }

    pub fn for_multibuffer(
        buffer: Entity<MultiBuffer>,
        project: Option<Entity<Project>>,
        show_excerpt_controls: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new(
            EditorMode::Full,
            buffer,
            project,
            show_excerpt_controls,
            window,
            cx,
        )
    }

    pub fn clone(&self, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let show_excerpt_controls = self.display_map.read(cx).show_excerpt_controls();
        let mut clone = Self::new(
            self.mode,
            self.buffer.clone(),
            self.project.clone(),
            show_excerpt_controls,
            window,
            cx,
        );
        self.display_map.update(cx, |display_map, cx| {
            let snapshot = display_map.snapshot(cx);
            clone.display_map.update(cx, |display_map, cx| {
                display_map.set_state(&snapshot, cx);
            });
        });
        clone.selections.clone_state(&self.selections);
        clone.scroll_manager.clone_state(&self.scroll_manager);
        clone.searchable = self.searchable;
        clone
    }

    pub fn new(
        mode: EditorMode,
        buffer: Entity<MultiBuffer>,
        project: Option<Entity<Project>>,
        show_excerpt_controls: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let editor = cx.entity().downgrade();
        let fold_placeholder = FoldPlaceholder {
            constrain_width: true,
            render: Arc::new(move |fold_id, fold_range, _, cx| {
                let editor = editor.clone();
                div()
                    .id(fold_id)
                    .bg(cx.theme().colors().ghost_element_background)
                    .hover(|style| style.bg(cx.theme().colors().ghost_element_hover))
                    .active(|style| style.bg(cx.theme().colors().ghost_element_active))
                    .rounded_sm()
                    .size_full()
                    .cursor_pointer()
                    .child("⋯")
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, _window, cx| {
                        editor
                            .update(cx, |editor, cx| {
                                editor.unfold_ranges(
                                    &[fold_range.start..fold_range.end],
                                    true,
                                    false,
                                    cx,
                                );
                                cx.stop_propagation();
                            })
                            .ok();
                    })
                    .into_any()
            }),
            merge_adjacent: true,
            ..Default::default()
        };
        let display_map = cx.new(|cx| {
            DisplayMap::new(
                buffer.clone(),
                style.font(),
                font_size,
                None,
                show_excerpt_controls,
                FILE_HEADER_HEIGHT,
                MULTI_BUFFER_EXCERPT_HEADER_HEIGHT,
                MULTI_BUFFER_EXCERPT_FOOTER_HEIGHT,
                fold_placeholder,
                cx,
            )
        });

        let selections = SelectionsCollection::new(display_map.clone(), buffer.clone());

        let blink_manager = cx.new(|cx| BlinkManager::new(CURSOR_BLINK_INTERVAL, cx));

        let soft_wrap_mode_override = matches!(mode, EditorMode::SingleLine { .. })
            .then(|| language_settings::SoftWrap::None);

        let mut project_subscriptions = Vec::new();
        if mode == EditorMode::Full {
            if let Some(project) = project.as_ref() {
                if buffer.read(cx).is_singleton() {
                    project_subscriptions.push(cx.observe_in(project, window, |_, _, _, cx| {
                        cx.emit(EditorEvent::TitleChanged);
                    }));
                }
                project_subscriptions.push(cx.subscribe_in(
                    project,
                    window,
                    |editor, _, event, window, cx| {
                        if let project::Event::RefreshInlayHints = event {
                            editor
                                .refresh_inlay_hints(InlayHintRefreshReason::RefreshRequested, cx);
                        } else if let project::Event::SnippetEdit(id, snippet_edits) = event {
                            if let Some(buffer) = editor.buffer.read(cx).buffer(*id) {
                                let focus_handle = editor.focus_handle(cx);
                                if focus_handle.is_focused(window) {
                                    let snapshot = buffer.read(cx).snapshot();
                                    for (range, snippet) in snippet_edits {
                                        let editor_range =
                                            language::range_from_lsp(*range).to_offset(&snapshot);
                                        editor
                                            .insert_snippet(
                                                &[editor_range],
                                                snippet.clone(),
                                                window,
                                                cx,
                                            )
                                            .ok();
                                    }
                                }
                            }
                        }
                    },
                ));
                if let Some(task_inventory) = project
                    .read(cx)
                    .task_store()
                    .read(cx)
                    .task_inventory()
                    .cloned()
                {
                    project_subscriptions.push(cx.observe_in(
                        &task_inventory,
                        window,
                        |editor, _, window, cx| {
                            editor.tasks_update_task = Some(editor.refresh_runnables(window, cx));
                        },
                    ));
                }
            }
        }

        let buffer_snapshot = buffer.read(cx).snapshot(cx);

        let inlay_hint_settings =
            inlay_hint_settings(selections.newest_anchor().head(), &buffer_snapshot, cx);
        let focus_handle = cx.focus_handle();
        cx.on_focus(&focus_handle, window, Self::handle_focus)
            .detach();
        cx.on_focus_in(&focus_handle, window, Self::handle_focus_in)
            .detach();
        cx.on_focus_out(&focus_handle, window, Self::handle_focus_out)
            .detach();
        cx.on_blur(&focus_handle, window, Self::handle_blur)
            .detach();

        let show_indent_guides = if matches!(mode, EditorMode::SingleLine { .. }) {
            Some(false)
        } else {
            None
        };

        let mut code_action_providers = Vec::new();
        if let Some(project) = project.clone() {
            get_uncommitted_diff_for_buffer(
                &project,
                buffer.read(cx).all_buffers(),
                buffer.clone(),
                cx,
            );
            code_action_providers.push(Rc::new(project) as Rc<_>);
        }

        let mut this = Self {
            focus_handle,
            show_cursor_when_unfocused: false,
            last_focused_descendant: None,
            buffer: buffer.clone(),
            display_map: display_map.clone(),
            selections,
            scroll_manager: ScrollManager::new(cx),
            columnar_selection_tail: None,
            add_selections_state: None,
            select_next_state: None,
            select_prev_state: None,
            selection_history: Default::default(),
            autoclose_regions: Default::default(),
            snippet_stack: Default::default(),
            select_larger_syntax_node_stack: Vec::new(),
            ime_transaction: Default::default(),
            active_diagnostics: None,
            soft_wrap_mode_override,
            completion_provider: project.clone().map(|project| Box::new(project) as _),
            semantics_provider: project.clone().map(|project| Rc::new(project) as _),
            collaboration_hub: project.clone().map(|project| Box::new(project) as _),
            project,
            blink_manager: blink_manager.clone(),
            show_local_selections: true,
            show_scrollbars: true,
            mode,
            show_breadcrumbs: EditorSettings::get_global(cx).toolbar.breadcrumbs,
            show_gutter: mode == EditorMode::Full,
            show_line_numbers: None,
            use_relative_line_numbers: None,
            show_git_diff_gutter: None,
            show_code_actions: None,
            show_runnables: None,
            show_wrap_guides: None,
            show_indent_guides,
            placeholder_text: None,
            highlight_order: 0,
            highlighted_rows: HashMap::default(),
            background_highlights: Default::default(),
            gutter_highlights: TreeMap::default(),
            scrollbar_marker_state: ScrollbarMarkerState::default(),
            active_indent_guides_state: ActiveIndentGuidesState::default(),
            nav_history: None,
            context_menu: RefCell::new(None),
            mouse_context_menu: None,
            completion_tasks: Default::default(),
            signature_help_state: SignatureHelpState::default(),
            auto_signature_help: None,
            find_all_references_task_sources: Vec::new(),
            next_completion_id: 0,
            next_inlay_id: 0,
            code_action_providers,
            available_code_actions: Default::default(),
            code_actions_task: Default::default(),
            document_highlights_task: Default::default(),
            linked_editing_range_task: Default::default(),
            pending_rename: Default::default(),
            searchable: true,
            cursor_shape: EditorSettings::get_global(cx)
                .cursor_shape
                .unwrap_or_default(),
            current_line_highlight: None,
            autoindent_mode: Some(AutoindentMode::EachLine),
            collapse_matches: false,
            workspace: None,
            input_enabled: true,
            use_modal_editing: mode == EditorMode::Full,
            read_only: false,
            use_autoclose: true,
            use_auto_surround: true,
            auto_replace_emoji_shortcode: false,
            leader_peer_id: None,
            remote_id: None,
            hover_state: Default::default(),
            pending_mouse_down: None,
            hovered_link_state: Default::default(),
            inline_completion_provider: None,
            active_inline_completion: None,
            stale_inline_completion_in_menu: None,
            previewing_inline_completion: false,
            inlay_hint_cache: InlayHintCache::new(inlay_hint_settings),

            gutter_hovered: false,
            pixel_position_of_newest_cursor: None,
            last_bounds: None,
            last_position_map: None,
            expect_bounds_change: None,
            gutter_dimensions: GutterDimensions::default(),
            style: None,
            show_cursor_names: false,
            hovered_cursors: Default::default(),
            next_editor_action_id: EditorActionId::default(),
            editor_actions: Rc::default(),
            show_inline_completions_override: None,
            show_inline_completions: true,
            menu_inline_completions_policy: MenuInlineCompletionsPolicy::ByProvider,
            custom_context_menu: None,
            show_git_blame_gutter: false,
            show_git_blame_inline: false,
            show_selection_menu: None,
            show_git_blame_inline_delay_task: None,
            git_blame_inline_enabled: ProjectSettings::get_global(cx).git.inline_blame_enabled(),
            serialize_dirty_buffers: ProjectSettings::get_global(cx)
                .session
                .restore_unsaved_buffers,
            blame: None,
            blame_subscription: None,
            tasks: Default::default(),
            _subscriptions: vec![
                cx.observe(&buffer, Self::on_buffer_changed),
                cx.subscribe_in(&buffer, window, Self::on_buffer_event),
                cx.observe_in(&display_map, window, Self::on_display_map_changed),
                cx.observe(&blink_manager, |_, _, cx| cx.notify()),
                cx.observe_global_in::<SettingsStore>(window, Self::settings_changed),
                cx.observe_window_activation(window, |editor, window, cx| {
                    let active = window.is_window_active();
                    editor.blink_manager.update(cx, |blink_manager, cx| {
                        if active {
                            blink_manager.enable(cx);
                        } else {
                            blink_manager.disable(cx);
                        }
                    });
                }),
            ],
            tasks_update_task: None,
            linked_edit_ranges: Default::default(),
            in_project_search: false,
            previous_search_ranges: None,
            breadcrumb_header: None,
            focused_block: None,
            next_scroll_position: NextScrollCursorCenterTopBottom::default(),
            addons: HashMap::default(),
            registered_buffers: HashMap::default(),
            _scroll_cursor_center_top_bottom_task: Task::ready(()),
            selection_mark_mode: false,
            toggle_fold_multiple_buffers: Task::ready(()),
            text_style_refinement: None,
        };
        this.tasks_update_task = Some(this.refresh_runnables(window, cx));
        this._subscriptions.extend(project_subscriptions);

        this.end_selection(window, cx);
        this.scroll_manager.show_scrollbar(window, cx);

        if mode == EditorMode::Full {
            let should_auto_hide_scrollbars = cx.should_auto_hide_scrollbars();
            cx.set_global(ScrollbarAutoHide(should_auto_hide_scrollbars));

            if this.git_blame_inline_enabled {
                this.git_blame_inline_enabled = true;
                this.start_git_blame_inline(false, window, cx);
            }

            if let Some(buffer) = buffer.read(cx).as_singleton() {
                if let Some(project) = this.project.as_ref() {
                    let lsp_store = project.read(cx).lsp_store();
                    let handle = lsp_store.update(cx, |lsp_store, cx| {
                        lsp_store.register_buffer_with_language_servers(&buffer, cx)
                    });
                    this.registered_buffers
                        .insert(buffer.read(cx).remote_id(), handle);
                }
            }
        }

        this.report_editor_event("Editor Opened", None, cx);
        this
    }

    pub fn mouse_menu_is_focused(&self, window: &mut Window, cx: &mut App) -> bool {
        self.mouse_context_menu
            .as_ref()
            .is_some_and(|menu| menu.context_menu.focus_handle(cx).is_focused(window))
    }

    fn key_context(&self, window: &mut Window, cx: &mut Context<Self>) -> KeyContext {
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add("Editor");
        let mode = match self.mode {
            EditorMode::SingleLine { .. } => "single_line",
            EditorMode::AutoHeight { .. } => "auto_height",
            EditorMode::Full => "full",
        };

        if EditorSettings::jupyter_enabled(cx) {
            key_context.add("jupyter");
        }

        key_context.set("mode", mode);
        if self.pending_rename.is_some() {
            key_context.add("renaming");
        }

        let mut showing_completions = false;

        match self.context_menu.borrow().as_ref() {
            Some(CodeContextMenu::Completions(_)) => {
                key_context.add("menu");
                key_context.add("showing_completions");
                showing_completions = true;
            }
            Some(CodeContextMenu::CodeActions(_)) => {
                key_context.add("menu");
                key_context.add("showing_code_actions")
            }
            None => {}
        }

        // Disable vim contexts when a sub-editor (e.g. rename/inline assistant) is focused.
        if !self.focus_handle(cx).contains_focused(window, cx)
            || (self.is_focused(window) || self.mouse_menu_is_focused(window, cx))
        {
            for addon in self.addons.values() {
                addon.extend_key_context(&mut key_context, cx)
            }
        }

        if let Some(extension) = self
            .buffer
            .read(cx)
            .as_singleton()
            .and_then(|buffer| buffer.read(cx).file()?.path().extension()?.to_str())
        {
            key_context.set("extension", extension.to_string());
        }

        if self.has_active_inline_completion() {
            key_context.add("copilot_suggestion");
            key_context.add("inline_completion");

            if showing_completions || self.inline_completion_requires_modifier(cx) {
                key_context.add("inline_completion_requires_modifier");
            }
        }

        if self.selection_mark_mode {
            key_context.add("selection_mode");
        }

        key_context
    }

    pub fn new_file(
        workspace: &mut Workspace,
        _: &workspace::NewFile,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        Self::new_in_workspace(workspace, window, cx).detach_and_prompt_err(
            "Failed to create buffer",
            window,
            cx,
            |e, _, _| match e.error_code() {
                ErrorCode::RemoteUpgradeRequired => Some(format!(
                "The remote instance of Zed does not support this yet. It must be upgraded to {}",
                e.error_tag("required").unwrap_or("the latest version")
            )),
                _ => None,
            },
        );
    }

    pub fn new_in_workspace(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Task<Result<Entity<Editor>>> {
        let project = workspace.project().clone();
        let create = project.update(cx, |project, cx| project.create_buffer(cx));

        cx.spawn_in(window, |workspace, mut cx| async move {
            let buffer = create.await?;
            workspace.update_in(&mut cx, |workspace, window, cx| {
                let editor =
                    cx.new(|cx| Editor::for_buffer(buffer, Some(project.clone()), window, cx));
                workspace.add_item_to_active_pane(Box::new(editor.clone()), None, true, window, cx);
                editor
            })
        })
    }

    fn new_file_vertical(
        workspace: &mut Workspace,
        _: &workspace::NewFileSplitVertical,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        Self::new_file_in_direction(workspace, SplitDirection::vertical(cx), window, cx)
    }

    fn new_file_horizontal(
        workspace: &mut Workspace,
        _: &workspace::NewFileSplitHorizontal,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        Self::new_file_in_direction(workspace, SplitDirection::horizontal(cx), window, cx)
    }

    fn new_file_in_direction(
        workspace: &mut Workspace,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let project = workspace.project().clone();
        let create = project.update(cx, |project, cx| project.create_buffer(cx));

        cx.spawn_in(window, |workspace, mut cx| async move {
            let buffer = create.await?;
            workspace.update_in(&mut cx, move |workspace, window, cx| {
                workspace.split_item(
                    direction,
                    Box::new(
                        cx.new(|cx| Editor::for_buffer(buffer, Some(project.clone()), window, cx)),
                    ),
                    window,
                    cx,
                )
            })?;
            anyhow::Ok(())
        })
        .detach_and_prompt_err("Failed to create buffer", window, cx, |e, _, _| {
            match e.error_code() {
                ErrorCode::RemoteUpgradeRequired => Some(format!(
                "The remote instance of Zed does not support this yet. It must be upgraded to {}",
                e.error_tag("required").unwrap_or("the latest version")
            )),
                _ => None,
            }
        });
    }

    pub fn leader_peer_id(&self) -> Option<PeerId> {
        self.leader_peer_id
    }

    pub fn buffer(&self) -> &Entity<MultiBuffer> {
        &self.buffer
    }

    pub fn workspace(&self) -> Option<Entity<Workspace>> {
        self.workspace.as_ref()?.0.upgrade()
    }

    pub fn title<'a>(&self, cx: &'a App) -> Cow<'a, str> {
        self.buffer().read(cx).title(cx)
    }

    pub fn snapshot(&self, window: &mut Window, cx: &mut App) -> EditorSnapshot {
        let git_blame_gutter_max_author_length = self
            .render_git_blame_gutter(cx)
            .then(|| {
                if let Some(blame) = self.blame.as_ref() {
                    let max_author_length =
                        blame.update(cx, |blame, cx| blame.max_author_length(cx));
                    Some(max_author_length)
                } else {
                    None
                }
            })
            .flatten();

        EditorSnapshot {
            mode: self.mode,
            show_gutter: self.show_gutter,
            show_line_numbers: self.show_line_numbers,
            show_git_diff_gutter: self.show_git_diff_gutter,
            show_code_actions: self.show_code_actions,
            show_runnables: self.show_runnables,
            git_blame_gutter_max_author_length,
            display_snapshot: self.display_map.update(cx, |map, cx| map.snapshot(cx)),
            scroll_anchor: self.scroll_manager.anchor(),
            ongoing_scroll: self.scroll_manager.ongoing_scroll(),
            placeholder_text: self.placeholder_text.clone(),
            is_focused: self.focus_handle.is_focused(window),
            current_line_highlight: self
                .current_line_highlight
                .unwrap_or_else(|| EditorSettings::get_global(cx).current_line_highlight),
            gutter_hovered: self.gutter_hovered,
        }
    }

    pub fn language_at<T: ToOffset>(&self, point: T, cx: &App) -> Option<Arc<Language>> {
        self.buffer.read(cx).language_at(point, cx)
    }

    pub fn file_at<T: ToOffset>(&self, point: T, cx: &App) -> Option<Arc<dyn language::File>> {
        self.buffer.read(cx).read(cx).file_at(point).cloned()
    }

    pub fn active_excerpt(
        &self,
        cx: &App,
    ) -> Option<(ExcerptId, Entity<Buffer>, Range<text::Anchor>)> {
        self.buffer
            .read(cx)
            .excerpt_containing(self.selections.newest_anchor().head(), cx)
    }

    pub fn mode(&self) -> EditorMode {
        self.mode
    }

    pub fn collaboration_hub(&self) -> Option<&dyn CollaborationHub> {
        self.collaboration_hub.as_deref()
    }

    pub fn set_collaboration_hub(&mut self, hub: Box<dyn CollaborationHub>) {
        self.collaboration_hub = Some(hub);
    }

    pub fn set_in_project_search(&mut self, in_project_search: bool) {
        self.in_project_search = in_project_search;
    }

    pub fn set_custom_context_menu(
        &mut self,
        f: impl 'static
            + Fn(
                &mut Self,
                DisplayPoint,
                &mut Window,
                &mut Context<Self>,
            ) -> Option<Entity<ui::ContextMenu>>,
    ) {
        self.custom_context_menu = Some(Box::new(f))
    }

    pub fn set_completion_provider(&mut self, provider: Option<Box<dyn CompletionProvider>>) {
        self.completion_provider = provider;
    }

    pub fn semantics_provider(&self) -> Option<Rc<dyn SemanticsProvider>> {
        self.semantics_provider.clone()
    }

    pub fn set_semantics_provider(&mut self, provider: Option<Rc<dyn SemanticsProvider>>) {
        self.semantics_provider = provider;
    }

    pub fn set_inline_completion_provider<T>(
        &mut self,
        provider: Option<Entity<T>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) where
        T: InlineCompletionProvider,
    {
        self.inline_completion_provider =
            provider.map(|provider| RegisteredInlineCompletionProvider {
                _subscription: cx.observe_in(&provider, window, |this, _, window, cx| {
                    if this.focus_handle.is_focused(window) {
                        this.update_visible_inline_completion(window, cx);
                    }
                }),
                provider: Arc::new(provider),
            });
        self.refresh_inline_completion(false, false, window, cx);
    }

    pub fn placeholder_text(&self) -> Option<&str> {
        self.placeholder_text.as_deref()
    }

    pub fn set_placeholder_text(
        &mut self,
        placeholder_text: impl Into<Arc<str>>,
        cx: &mut Context<Self>,
    ) {
        let placeholder_text = Some(placeholder_text.into());
        if self.placeholder_text != placeholder_text {
            self.placeholder_text = placeholder_text;
            cx.notify();
        }
    }

    pub fn set_cursor_shape(&mut self, cursor_shape: CursorShape, cx: &mut Context<Self>) {
        self.cursor_shape = cursor_shape;

        // Disrupt blink for immediate user feedback that the cursor shape has changed
        self.blink_manager.update(cx, BlinkManager::show_cursor);

        cx.notify();
    }

    pub fn set_current_line_highlight(
        &mut self,
        current_line_highlight: Option<CurrentLineHighlight>,
    ) {
        self.current_line_highlight = current_line_highlight;
    }

    pub fn set_collapse_matches(&mut self, collapse_matches: bool) {
        self.collapse_matches = collapse_matches;
    }

    pub fn register_buffers_with_language_servers(&mut self, cx: &mut Context<Self>) {
        let buffers = self.buffer.read(cx).all_buffers();
        let Some(lsp_store) = self.lsp_store(cx) else {
            return;
        };
        lsp_store.update(cx, |lsp_store, cx| {
            for buffer in buffers {
                self.registered_buffers
                    .entry(buffer.read(cx).remote_id())
                    .or_insert_with(|| {
                        lsp_store.register_buffer_with_language_servers(&buffer, cx)
                    });
            }
        })
    }

    pub fn range_for_match<T: std::marker::Copy>(&self, range: &Range<T>) -> Range<T> {
        if self.collapse_matches {
            return range.start..range.start;
        }
        range.clone()
    }

    pub fn set_clip_at_line_ends(&mut self, clip: bool, cx: &mut Context<Self>) {
        if self.display_map.read(cx).clip_at_line_ends != clip {
            self.display_map
                .update(cx, |map, _| map.clip_at_line_ends = clip);
        }
    }

    pub fn set_input_enabled(&mut self, input_enabled: bool) {
        self.input_enabled = input_enabled;
    }

    pub fn set_show_inline_completions_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.show_inline_completions = enabled;
        if !self.show_inline_completions {
            self.take_active_inline_completion(cx);
            cx.notify();
        }
    }

    pub fn set_menu_inline_completions_policy(&mut self, value: MenuInlineCompletionsPolicy) {
        self.menu_inline_completions_policy = value;
    }

    pub fn set_autoindent(&mut self, autoindent: bool) {
        if autoindent {
            self.autoindent_mode = Some(AutoindentMode::EachLine);
        } else {
            self.autoindent_mode = None;
        }
    }

    pub fn read_only(&self, cx: &App) -> bool {
        self.read_only || self.buffer.read(cx).read_only()
    }

    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    pub fn set_use_autoclose(&mut self, autoclose: bool) {
        self.use_autoclose = autoclose;
    }

    pub fn set_use_auto_surround(&mut self, auto_surround: bool) {
        self.use_auto_surround = auto_surround;
    }

    pub fn set_auto_replace_emoji_shortcode(&mut self, auto_replace: bool) {
        self.auto_replace_emoji_shortcode = auto_replace;
    }

    pub fn toggle_inline_completions(
        &mut self,
        _: &ToggleInlineCompletions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.show_inline_completions_override.is_some() {
            self.set_show_inline_completions(None, window, cx);
        } else {
            let cursor = self.selections.newest_anchor().head();
            if let Some((buffer, cursor_buffer_position)) =
                self.buffer.read(cx).text_anchor_for_position(cursor, cx)
            {
                let show_inline_completions = !self.should_show_inline_completions_in_buffer(
                    &buffer,
                    cursor_buffer_position,
                    cx,
                );
                self.set_show_inline_completions(Some(show_inline_completions), window, cx);
            }
        }
    }

    pub fn set_show_inline_completions(
        &mut self,
        show_inline_completions: Option<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_inline_completions_override = show_inline_completions;
        self.refresh_inline_completion(false, true, window, cx);
    }

    fn inline_completions_disabled_in_scope(
        &self,
        buffer: &Entity<Buffer>,
        buffer_position: language::Anchor,
        cx: &App,
    ) -> bool {
        let snapshot = buffer.read(cx).snapshot();
        let settings = snapshot.settings_at(buffer_position, cx);

        let Some(scope) = snapshot.language_scope_at(buffer_position) else {
            return false;
        };

        scope.override_name().map_or(false, |scope_name| {
            settings
                .inline_completions_disabled_in
                .iter()
                .any(|s| s == scope_name)
        })
    }

    pub fn set_use_modal_editing(&mut self, to: bool) {
        self.use_modal_editing = to;
    }

    pub fn use_modal_editing(&self) -> bool {
        self.use_modal_editing
    }

    fn selections_did_change(
        &mut self,
        local: bool,
        old_cursor_position: &Anchor,
        show_completions: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.invalidate_character_coordinates();

        // Copy selections to primary selection buffer
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        if local {
            let selections = self.selections.all::<usize>(cx);
            let buffer_handle = self.buffer.read(cx).read(cx);

            let mut text = String::new();
            for (index, selection) in selections.iter().enumerate() {
                let text_for_selection = buffer_handle
                    .text_for_range(selection.start..selection.end)
                    .collect::<String>();

                text.push_str(&text_for_selection);
                if index != selections.len() - 1 {
                    text.push('\n');
                }
            }

            if !text.is_empty() {
                cx.write_to_primary(ClipboardItem::new_string(text));
            }
        }

        if self.focus_handle.is_focused(window) && self.leader_peer_id.is_none() {
            self.buffer.update(cx, |buffer, cx| {
                buffer.set_active_selections(
                    &self.selections.disjoint_anchors(),
                    self.selections.line_mode,
                    self.cursor_shape,
                    cx,
                )
            });
        }
        let display_map = self
            .display_map
            .update(cx, |display_map, cx| display_map.snapshot(cx));
        let buffer = &display_map.buffer_snapshot;
        self.add_selections_state = None;
        self.select_next_state = None;
        self.select_prev_state = None;
        self.select_larger_syntax_node_stack.clear();
        self.invalidate_autoclose_regions(&self.selections.disjoint_anchors(), buffer);
        self.snippet_stack
            .invalidate(&self.selections.disjoint_anchors(), buffer);
        self.take_rename(false, window, cx);

        let new_cursor_position = self.selections.newest_anchor().head();

        self.push_to_nav_history(
            *old_cursor_position,
            Some(new_cursor_position.to_point(buffer)),
            cx,
        );

        if local {
            let new_cursor_position = self.selections.newest_anchor().head();
            let mut context_menu = self.context_menu.borrow_mut();
            let completion_menu = match context_menu.as_ref() {
                Some(CodeContextMenu::Completions(menu)) => Some(menu),
                _ => {
                    *context_menu = None;
                    None
                }
            };

            if let Some(completion_menu) = completion_menu {
                let cursor_position = new_cursor_position.to_offset(buffer);
                let (word_range, kind) =
                    buffer.surrounding_word(completion_menu.initial_position, true);
                if kind == Some(CharKind::Word)
                    && word_range.to_inclusive().contains(&cursor_position)
                {
                    let mut completion_menu = completion_menu.clone();
                    drop(context_menu);

                    let query = Self::completion_query(buffer, cursor_position);
                    cx.spawn(move |this, mut cx| async move {
                        completion_menu
                            .filter(query.as_deref(), cx.background_executor().clone())
                            .await;

                        this.update(&mut cx, |this, cx| {
                            let mut context_menu = this.context_menu.borrow_mut();
                            let Some(CodeContextMenu::Completions(menu)) = context_menu.as_ref()
                            else {
                                return;
                            };

                            if menu.id > completion_menu.id {
                                return;
                            }

                            *context_menu = Some(CodeContextMenu::Completions(completion_menu));
                            drop(context_menu);
                            cx.notify();
                        })
                    })
                    .detach();

                    if show_completions {
                        self.show_completions(&ShowCompletions { trigger: None }, window, cx);
                    }
                } else {
                    drop(context_menu);
                    self.hide_context_menu(window, cx);
                }
            } else {
                drop(context_menu);
            }

            hide_hover(self, cx);

            if old_cursor_position.to_display_point(&display_map).row()
                != new_cursor_position.to_display_point(&display_map).row()
            {
                self.available_code_actions.take();
            }
            self.refresh_code_actions(window, cx);
            self.refresh_document_highlights(cx);
            refresh_matching_bracket_highlights(self, window, cx);
            self.update_visible_inline_completion(window, cx);
            linked_editing_ranges::refresh_linked_ranges(self, window, cx);
            if self.git_blame_inline_enabled {
                self.start_inline_blame_timer(window, cx);
            }
        }

        self.blink_manager.update(cx, BlinkManager::pause_blinking);
        cx.emit(EditorEvent::SelectionsChanged { local });

        if self.selections.disjoint_anchors().len() == 1 {
            cx.emit(SearchEvent::ActiveMatchChanged)
        }
        cx.notify();
    }

    pub fn change_selections<R>(
        &mut self,
        autoscroll: Option<Autoscroll>,
        window: &mut Window,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut MutableSelectionsCollection<'_>) -> R,
    ) -> R {
        self.change_selections_inner(autoscroll, true, window, cx, change)
    }

    pub fn change_selections_inner<R>(
        &mut self,
        autoscroll: Option<Autoscroll>,
        request_completions: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut MutableSelectionsCollection<'_>) -> R,
    ) -> R {
        let old_cursor_position = self.selections.newest_anchor().head();
        self.push_to_selection_history();

        let (changed, result) = self.selections.change_with(cx, change);

        if changed {
            if let Some(autoscroll) = autoscroll {
                self.request_autoscroll(autoscroll, cx);
            }
            self.selections_did_change(true, &old_cursor_position, request_completions, window, cx);

            if self.should_open_signature_help_automatically(
                &old_cursor_position,
                self.signature_help_state.backspace_pressed(),
                cx,
            ) {
                self.show_signature_help(&ShowSignatureHelp, window, cx);
            }
            self.signature_help_state.set_backspace_pressed(false);
        }

        result
    }

    pub fn edit<I, S, T>(&mut self, edits: I, cx: &mut Context<Self>)
    where
        I: IntoIterator<Item = (Range<S>, T)>,
        S: ToOffset,
        T: Into<Arc<str>>,
    {
        if self.read_only(cx) {
            return;
        }

        self.buffer
            .update(cx, |buffer, cx| buffer.edit(edits, None, cx));
    }

    pub fn edit_with_autoindent<I, S, T>(&mut self, edits: I, cx: &mut Context<Self>)
    where
        I: IntoIterator<Item = (Range<S>, T)>,
        S: ToOffset,
        T: Into<Arc<str>>,
    {
        if self.read_only(cx) {
            return;
        }

        self.buffer.update(cx, |buffer, cx| {
            buffer.edit(edits, self.autoindent_mode.clone(), cx)
        });
    }

    pub fn edit_with_block_indent<I, S, T>(
        &mut self,
        edits: I,
        original_indent_columns: Vec<u32>,
        cx: &mut Context<Self>,
    ) where
        I: IntoIterator<Item = (Range<S>, T)>,
        S: ToOffset,
        T: Into<Arc<str>>,
    {
        if self.read_only(cx) {
            return;
        }

        self.buffer.update(cx, |buffer, cx| {
            buffer.edit(
                edits,
                Some(AutoindentMode::Block {
                    original_indent_columns,
                }),
                cx,
            )
        });
    }

    fn select(&mut self, phase: SelectPhase, window: &mut Window, cx: &mut Context<Self>) {
        self.hide_context_menu(window, cx);

        match phase {
            SelectPhase::Begin {
                position,
                add,
                click_count,
            } => self.begin_selection(position, add, click_count, window, cx),
            SelectPhase::BeginColumnar {
                position,
                goal_column,
                reset,
            } => self.begin_columnar_selection(position, goal_column, reset, window, cx),
            SelectPhase::Extend {
                position,
                click_count,
            } => self.extend_selection(position, click_count, window, cx),
            SelectPhase::Update {
                position,
                goal_column,
                scroll_delta,
            } => self.update_selection(position, goal_column, scroll_delta, window, cx),
            SelectPhase::End => self.end_selection(window, cx),
        }
    }

    fn extend_selection(
        &mut self,
        position: DisplayPoint,
        click_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let tail = self.selections.newest::<usize>(cx).tail();
        self.begin_selection(position, false, click_count, window, cx);

        let position = position.to_offset(&display_map, Bias::Left);
        let tail_anchor = display_map.buffer_snapshot.anchor_before(tail);

        let mut pending_selection = self
            .selections
            .pending_anchor()
            .expect("extend_selection not called with pending selection");
        if position >= tail {
            pending_selection.start = tail_anchor;
        } else {
            pending_selection.end = tail_anchor;
            pending_selection.reversed = true;
        }

        let mut pending_mode = self.selections.pending_mode().unwrap();
        match &mut pending_mode {
            SelectMode::Word(range) | SelectMode::Line(range) => *range = tail_anchor..tail_anchor,
            _ => {}
        }

        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.set_pending(pending_selection, pending_mode)
        });
    }

    fn begin_selection(
        &mut self,
        position: DisplayPoint,
        add: bool,
        click_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.focus_handle.is_focused(window) {
            self.last_focused_descendant = None;
            window.focus(&self.focus_handle);
        }

        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let buffer = &display_map.buffer_snapshot;
        let newest_selection = self.selections.newest_anchor().clone();
        let position = display_map.clip_point(position, Bias::Left);

        let start;
        let end;
        let mode;
        let mut auto_scroll;
        match click_count {
            1 => {
                start = buffer.anchor_before(position.to_point(&display_map));
                end = start;
                mode = SelectMode::Character;
                auto_scroll = true;
            }
            2 => {
                let range = movement::surrounding_word(&display_map, position);
                start = buffer.anchor_before(range.start.to_point(&display_map));
                end = buffer.anchor_before(range.end.to_point(&display_map));
                mode = SelectMode::Word(start..end);
                auto_scroll = true;
            }
            3 => {
                let position = display_map
                    .clip_point(position, Bias::Left)
                    .to_point(&display_map);
                let line_start = display_map.prev_line_boundary(position).0;
                let next_line_start = buffer.clip_point(
                    display_map.next_line_boundary(position).0 + Point::new(1, 0),
                    Bias::Left,
                );
                start = buffer.anchor_before(line_start);
                end = buffer.anchor_before(next_line_start);
                mode = SelectMode::Line(start..end);
                auto_scroll = true;
            }
            _ => {
                start = buffer.anchor_before(0);
                end = buffer.anchor_before(buffer.len());
                mode = SelectMode::All;
                auto_scroll = false;
            }
        }
        auto_scroll &= EditorSettings::get_global(cx).autoscroll_on_clicks;

        let point_to_delete: Option<usize> = {
            let selected_points: Vec<Selection<Point>> =
                self.selections.disjoint_in_range(start..end, cx);

            if !add || click_count > 1 {
                None
            } else if !selected_points.is_empty() {
                Some(selected_points[0].id)
            } else {
                let clicked_point_already_selected =
                    self.selections.disjoint.iter().find(|selection| {
                        selection.start.to_point(buffer) == start.to_point(buffer)
                            || selection.end.to_point(buffer) == end.to_point(buffer)
                    });

                clicked_point_already_selected.map(|selection| selection.id)
            }
        };

        let selections_count = self.selections.count();

        self.change_selections(auto_scroll.then(Autoscroll::newest), window, cx, |s| {
            if let Some(point_to_delete) = point_to_delete {
                s.delete(point_to_delete);

                if selections_count == 1 {
                    s.set_pending_anchor_range(start..end, mode);
                }
            } else {
                if !add {
                    s.clear_disjoint();
                } else if click_count > 1 {
                    s.delete(newest_selection.id)
                }

                s.set_pending_anchor_range(start..end, mode);
            }
        });
    }

    fn begin_columnar_selection(
        &mut self,
        position: DisplayPoint,
        goal_column: u32,
        reset: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.focus_handle.is_focused(window) {
            self.last_focused_descendant = None;
            window.focus(&self.focus_handle);
        }

        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));

        if reset {
            let pointer_position = display_map
                .buffer_snapshot
                .anchor_before(position.to_point(&display_map));

            self.change_selections(Some(Autoscroll::newest()), window, cx, |s| {
                s.clear_disjoint();
                s.set_pending_anchor_range(
                    pointer_position..pointer_position,
                    SelectMode::Character,
                );
            });
        }

        let tail = self.selections.newest::<Point>(cx).tail();
        self.columnar_selection_tail = Some(display_map.buffer_snapshot.anchor_before(tail));

        if !reset {
            self.select_columns(
                tail.to_display_point(&display_map),
                position,
                goal_column,
                &display_map,
                window,
                cx,
            );
        }
    }

    fn update_selection(
        &mut self,
        position: DisplayPoint,
        goal_column: u32,
        scroll_delta: gpui::Point<f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));

        if let Some(tail) = self.columnar_selection_tail.as_ref() {
            let tail = tail.to_display_point(&display_map);
            self.select_columns(tail, position, goal_column, &display_map, window, cx);
        } else if let Some(mut pending) = self.selections.pending_anchor() {
            let buffer = self.buffer.read(cx).snapshot(cx);
            let head;
            let tail;
            let mode = self.selections.pending_mode().unwrap();
            match &mode {
                SelectMode::Character => {
                    head = position.to_point(&display_map);
                    tail = pending.tail().to_point(&buffer);
                }
                SelectMode::Word(original_range) => {
                    let original_display_range = original_range.start.to_display_point(&display_map)
                        ..original_range.end.to_display_point(&display_map);
                    let original_buffer_range = original_display_range.start.to_point(&display_map)
                        ..original_display_range.end.to_point(&display_map);
                    if movement::is_inside_word(&display_map, position)
                        || original_display_range.contains(&position)
                    {
                        let word_range = movement::surrounding_word(&display_map, position);
                        if word_range.start < original_display_range.start {
                            head = word_range.start.to_point(&display_map);
                        } else {
                            head = word_range.end.to_point(&display_map);
                        }
                    } else {
                        head = position.to_point(&display_map);
                    }

                    if head <= original_buffer_range.start {
                        tail = original_buffer_range.end;
                    } else {
                        tail = original_buffer_range.start;
                    }
                }
                SelectMode::Line(original_range) => {
                    let original_range = original_range.to_point(&display_map.buffer_snapshot);

                    let position = display_map
                        .clip_point(position, Bias::Left)
                        .to_point(&display_map);
                    let line_start = display_map.prev_line_boundary(position).0;
                    let next_line_start = buffer.clip_point(
                        display_map.next_line_boundary(position).0 + Point::new(1, 0),
                        Bias::Left,
                    );

                    if line_start < original_range.start {
                        head = line_start
                    } else {
                        head = next_line_start
                    }

                    if head <= original_range.start {
                        tail = original_range.end;
                    } else {
                        tail = original_range.start;
                    }
                }
                SelectMode::All => {
                    return;
                }
            };

            if head < tail {
                pending.start = buffer.anchor_before(head);
                pending.end = buffer.anchor_before(tail);
                pending.reversed = true;
            } else {
                pending.start = buffer.anchor_before(tail);
                pending.end = buffer.anchor_before(head);
                pending.reversed = false;
            }

            self.change_selections(None, window, cx, |s| {
                s.set_pending(pending, mode);
            });
        } else {
            log::error!("update_selection dispatched with no pending selection");
            return;
        }

        self.apply_scroll_delta(scroll_delta, window, cx);
        cx.notify();
    }

    fn end_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.columnar_selection_tail.take();
        if self.selections.pending_anchor().is_some() {
            let selections = self.selections.all::<usize>(cx);
            self.change_selections(None, window, cx, |s| {
                s.select(selections);
                s.clear_pending();
            });
        }
    }

    fn select_columns(
        &mut self,
        tail: DisplayPoint,
        head: DisplayPoint,
        goal_column: u32,
        display_map: &DisplaySnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let start_row = cmp::min(tail.row(), head.row());
        let end_row = cmp::max(tail.row(), head.row());
        let start_column = cmp::min(tail.column(), goal_column);
        let end_column = cmp::max(tail.column(), goal_column);
        let reversed = start_column < tail.column();

        let selection_ranges = (start_row.0..=end_row.0)
            .map(DisplayRow)
            .filter_map(|row| {
                if start_column <= display_map.line_len(row) && !display_map.is_block_line(row) {
                    let start = display_map
                        .clip_point(DisplayPoint::new(row, start_column), Bias::Left)
                        .to_point(display_map);
                    let end = display_map
                        .clip_point(DisplayPoint::new(row, end_column), Bias::Right)
                        .to_point(display_map);
                    if reversed {
                        Some(end..start)
                    } else {
                        Some(start..end)
                    }
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        self.change_selections(None, window, cx, |s| {
            s.select_ranges(selection_ranges);
        });
        cx.notify();
    }

    pub fn has_pending_nonempty_selection(&self) -> bool {
        let pending_nonempty_selection = match self.selections.pending_anchor() {
            Some(Selection { start, end, .. }) => start != end,
            None => false,
        };

        pending_nonempty_selection
            || (self.columnar_selection_tail.is_some() && self.selections.disjoint.len() > 1)
    }

    pub fn has_pending_selection(&self) -> bool {
        self.selections.pending_anchor().is_some() || self.columnar_selection_tail.is_some()
    }

    pub fn cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        self.selection_mark_mode = false;

        if self.clear_expanded_diff_hunks(cx) {
            cx.notify();
            return;
        }
        if self.dismiss_menus_and_popups(true, window, cx) {
            return;
        }

        if self.mode == EditorMode::Full
            && self.change_selections(Some(Autoscroll::fit()), window, cx, |s| s.try_cancel())
        {
            return;
        }

        cx.propagate();
    }

    pub fn dismiss_menus_and_popups(
        &mut self,
        should_report_inline_completion_event: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.take_rename(false, window, cx).is_some() {
            return true;
        }

        if hide_hover(self, cx) {
            return true;
        }

        if self.hide_signature_help(cx, SignatureHelpHiddenBy::Escape) {
            return true;
        }

        if self.hide_context_menu(window, cx).is_some() {
            return true;
        }

        if self.mouse_context_menu.take().is_some() {
            return true;
        }

        if self.discard_inline_completion(should_report_inline_completion_event, cx) {
            return true;
        }

        if self.snippet_stack.pop().is_some() {
            return true;
        }

        if self.mode == EditorMode::Full && self.active_diagnostics.is_some() {
            self.dismiss_diagnostics(cx);
            return true;
        }

        false
    }

    fn linked_editing_ranges_for(
        &self,
        selection: Range<text::Anchor>,
        cx: &App,
    ) -> Option<HashMap<Entity<Buffer>, Vec<Range<text::Anchor>>>> {
        if self.linked_edit_ranges.is_empty() {
            return None;
        }
        let ((base_range, linked_ranges), buffer_snapshot, buffer) =
            selection.end.buffer_id.and_then(|end_buffer_id| {
                if selection.start.buffer_id != Some(end_buffer_id) {
                    return None;
                }
                let buffer = self.buffer.read(cx).buffer(end_buffer_id)?;
                let snapshot = buffer.read(cx).snapshot();
                self.linked_edit_ranges
                    .get(end_buffer_id, selection.start..selection.end, &snapshot)
                    .map(|ranges| (ranges, snapshot, buffer))
            })?;
        use text::ToOffset as TO;
        // find offset from the start of current range to current cursor position
        let start_byte_offset = TO::to_offset(&base_range.start, &buffer_snapshot);

        let start_offset = TO::to_offset(&selection.start, &buffer_snapshot);
        let start_difference = start_offset - start_byte_offset;
        let end_offset = TO::to_offset(&selection.end, &buffer_snapshot);
        let end_difference = end_offset - start_byte_offset;
        // Current range has associated linked ranges.
        let mut linked_edits = HashMap::<_, Vec<_>>::default();
        for range in linked_ranges.iter() {
            let start_offset = TO::to_offset(&range.start, &buffer_snapshot);
            let end_offset = start_offset + end_difference;
            let start_offset = start_offset + start_difference;
            if start_offset > buffer_snapshot.len() || end_offset > buffer_snapshot.len() {
                continue;
            }
            if self.selections.disjoint_anchor_ranges().any(|s| {
                if s.start.buffer_id != selection.start.buffer_id
                    || s.end.buffer_id != selection.end.buffer_id
                {
                    return false;
                }
                TO::to_offset(&s.start.text_anchor, &buffer_snapshot) <= end_offset
                    && TO::to_offset(&s.end.text_anchor, &buffer_snapshot) >= start_offset
            }) {
                continue;
            }
            let start = buffer_snapshot.anchor_after(start_offset);
            let end = buffer_snapshot.anchor_after(end_offset);
            linked_edits
                .entry(buffer.clone())
                .or_default()
                .push(start..end);
        }
        Some(linked_edits)
    }

    pub fn handle_input(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text: Arc<str> = text.into();

        if self.read_only(cx) {
            return;
        }

        let selections = self.selections.all_adjusted(cx);
        let mut bracket_inserted = false;
        let mut edits = Vec::new();
        let mut linked_edits = HashMap::<_, Vec<_>>::default();
        let mut new_selections = Vec::with_capacity(selections.len());
        let mut new_autoclose_regions = Vec::new();
        let snapshot = self.buffer.read(cx).read(cx);

        for (selection, autoclose_region) in
            self.selections_with_autoclose_regions(selections, &snapshot)
        {
            if let Some(scope) = snapshot.language_scope_at(selection.head()) {
                // Determine if the inserted text matches the opening or closing
                // bracket of any of this language's bracket pairs.
                let mut bracket_pair = None;
                let mut is_bracket_pair_start = false;
                let mut is_bracket_pair_end = false;
                if !text.is_empty() {
                    // `text` can be empty when a user is using IME (e.g. Chinese Wubi Simplified)
                    //  and they are removing the character that triggered IME popup.
                    for (pair, enabled) in scope.brackets() {
                        if !pair.close && !pair.surround {
                            continue;
                        }

                        if enabled && pair.start.ends_with(text.as_ref()) {
                            let prefix_len = pair.start.len() - text.len();
                            let preceding_text_matches_prefix = prefix_len == 0
                                || (selection.start.column >= (prefix_len as u32)
                                    && snapshot.contains_str_at(
                                        Point::new(
                                            selection.start.row,
                                            selection.start.column - (prefix_len as u32),
                                        ),
                                        &pair.start[..prefix_len],
                                    ));
                            if preceding_text_matches_prefix {
                                bracket_pair = Some(pair.clone());
                                is_bracket_pair_start = true;
                                break;
                            }
                        }
                        if pair.end.as_str() == text.as_ref() {
                            bracket_pair = Some(pair.clone());
                            is_bracket_pair_end = true;
                            break;
                        }
                    }
                }

                if let Some(bracket_pair) = bracket_pair {
                    let snapshot_settings = snapshot.settings_at(selection.start, cx);
                    let autoclose = self.use_autoclose && snapshot_settings.use_autoclose;
                    let auto_surround =
                        self.use_auto_surround && snapshot_settings.use_auto_surround;
                    if selection.is_empty() {
                        if is_bracket_pair_start {
                            // If the inserted text is a suffix of an opening bracket and the
                            // selection is preceded by the rest of the opening bracket, then
                            // insert the closing bracket.
                            let following_text_allows_autoclose = snapshot
                                .chars_at(selection.start)
                                .next()
                                .map_or(true, |c| scope.should_autoclose_before(c));

                            let is_closing_quote = if bracket_pair.end == bracket_pair.start
                                && bracket_pair.start.len() == 1
                            {
                                let target = bracket_pair.start.chars().next().unwrap();
                                let current_line_count = snapshot
                                    .reversed_chars_at(selection.start)
                                    .take_while(|&c| c != '\n')
                                    .filter(|&c| c == target)
                                    .count();
                                current_line_count % 2 == 1
                            } else {
                                false
                            };

                            if autoclose
                                && bracket_pair.close
                                && following_text_allows_autoclose
                                && !is_closing_quote
                            {
                                let anchor = snapshot.anchor_before(selection.end);
                                new_selections.push((selection.map(|_| anchor), text.len()));
                                new_autoclose_regions.push((
                                    anchor,
                                    text.len(),
                                    selection.id,
                                    bracket_pair.clone(),
                                ));
                                edits.push((
                                    selection.range(),
                                    format!("{}{}", text, bracket_pair.end).into(),
                                ));
                                bracket_inserted = true;
                                continue;
                            }
                        }

                        if let Some(region) = autoclose_region {
                            // If the selection is followed by an auto-inserted closing bracket,
                            // then don't insert that closing bracket again; just move the selection
                            // past the closing bracket.
                            let should_skip = selection.end == region.range.end.to_point(&snapshot)
                                && text.as_ref() == region.pair.end.as_str();
                            if should_skip {
                                let anchor = snapshot.anchor_after(selection.end);
                                new_selections
                                    .push((selection.map(|_| anchor), region.pair.end.len()));
                                continue;
                            }
                        }

                        let always_treat_brackets_as_autoclosed = snapshot
                            .settings_at(selection.start, cx)
                            .always_treat_brackets_as_autoclosed;
                        if always_treat_brackets_as_autoclosed
                            && is_bracket_pair_end
                            && snapshot.contains_str_at(selection.end, text.as_ref())
                        {
                            // Otherwise, when `always_treat_brackets_as_autoclosed` is set to `true
                            // and the inserted text is a closing bracket and the selection is followed
                            // by the closing bracket then move the selection past the closing bracket.
                            let anchor = snapshot.anchor_after(selection.end);
                            new_selections.push((selection.map(|_| anchor), text.len()));
                            continue;
                        }
                    }
                    // If an opening bracket is 1 character long and is typed while
                    // text is selected, then surround that text with the bracket pair.
                    else if auto_surround
                        && bracket_pair.surround
                        && is_bracket_pair_start
                        && bracket_pair.start.chars().count() == 1
                    {
                        edits.push((selection.start..selection.start, text.clone()));
                        edits.push((
                            selection.end..selection.end,
                            bracket_pair.end.as_str().into(),
                        ));
                        bracket_inserted = true;
                        new_selections.push((
                            Selection {
                                id: selection.id,
                                start: snapshot.anchor_after(selection.start),
                                end: snapshot.anchor_before(selection.end),
                                reversed: selection.reversed,
                                goal: selection.goal,
                            },
                            0,
                        ));
                        continue;
                    }
                }
            }

            if self.auto_replace_emoji_shortcode
                && selection.is_empty()
                && text.as_ref().ends_with(':')
            {
                if let Some(possible_emoji_short_code) =
                    Self::find_possible_emoji_shortcode_at_position(&snapshot, selection.start)
                {
                    if !possible_emoji_short_code.is_empty() {
                        if let Some(emoji) = emojis::get_by_shortcode(&possible_emoji_short_code) {
                            let emoji_shortcode_start = Point::new(
                                selection.start.row,
                                selection.start.column - possible_emoji_short_code.len() as u32 - 1,
                            );

                            // Remove shortcode from buffer
                            edits.push((
                                emoji_shortcode_start..selection.start,
                                "".to_string().into(),
                            ));
                            new_selections.push((
                                Selection {
                                    id: selection.id,
                                    start: snapshot.anchor_after(emoji_shortcode_start),
                                    end: snapshot.anchor_before(selection.start),
                                    reversed: selection.reversed,
                                    goal: selection.goal,
                                },
                                0,
                            ));

                            // Insert emoji
                            let selection_start_anchor = snapshot.anchor_after(selection.start);
                            new_selections.push((selection.map(|_| selection_start_anchor), 0));
                            edits.push((selection.start..selection.end, emoji.to_string().into()));

                            continue;
                        }
                    }
                }
            }

            // If not handling any auto-close operation, then just replace the selected
            // text with the given input and move the selection to the end of the
            // newly inserted text.
            let anchor = snapshot.anchor_after(selection.end);
            if !self.linked_edit_ranges.is_empty() {
                let start_anchor = snapshot.anchor_before(selection.start);

                let is_word_char = text.chars().next().map_or(true, |char| {
                    let classifier = snapshot.char_classifier_at(start_anchor.to_offset(&snapshot));
                    classifier.is_word(char)
                });

                if is_word_char {
                    if let Some(ranges) = self
                        .linked_editing_ranges_for(start_anchor.text_anchor..anchor.text_anchor, cx)
                    {
                        for (buffer, edits) in ranges {
                            linked_edits
                                .entry(buffer.clone())
                                .or_default()
                                .extend(edits.into_iter().map(|range| (range, text.clone())));
                        }
                    }
                }
            }

            new_selections.push((selection.map(|_| anchor), 0));
            edits.push((selection.start..selection.end, text.clone()));
        }

        drop(snapshot);

        self.transact(window, cx, |this, window, cx| {
            this.buffer.update(cx, |buffer, cx| {
                buffer.edit(edits, this.autoindent_mode.clone(), cx);
            });
            for (buffer, edits) in linked_edits {
                buffer.update(cx, |buffer, cx| {
                    let snapshot = buffer.snapshot();
                    let edits = edits
                        .into_iter()
                        .map(|(range, text)| {
                            use text::ToPoint as TP;
                            let end_point = TP::to_point(&range.end, &snapshot);
                            let start_point = TP::to_point(&range.start, &snapshot);
                            (start_point..end_point, text)
                        })
                        .sorted_by_key(|(range, _)| range.start)
                        .collect::<Vec<_>>();
                    buffer.edit(edits, None, cx);
                })
            }
            let new_anchor_selections = new_selections.iter().map(|e| &e.0);
            let new_selection_deltas = new_selections.iter().map(|e| e.1);
            let map = this.display_map.update(cx, |map, cx| map.snapshot(cx));
            let new_selections = resolve_selections::<usize, _>(new_anchor_selections, &map)
                .zip(new_selection_deltas)
                .map(|(selection, delta)| Selection {
                    id: selection.id,
                    start: selection.start + delta,
                    end: selection.end + delta,
                    reversed: selection.reversed,
                    goal: SelectionGoal::None,
                })
                .collect::<Vec<_>>();

            let mut i = 0;
            for (position, delta, selection_id, pair) in new_autoclose_regions {
                let position = position.to_offset(&map.buffer_snapshot) + delta;
                let start = map.buffer_snapshot.anchor_before(position);
                let end = map.buffer_snapshot.anchor_after(position);
                while let Some(existing_state) = this.autoclose_regions.get(i) {
                    match existing_state.range.start.cmp(&start, &map.buffer_snapshot) {
                        Ordering::Less => i += 1,
                        Ordering::Greater => break,
                        Ordering::Equal => {
                            match end.cmp(&existing_state.range.end, &map.buffer_snapshot) {
                                Ordering::Less => i += 1,
                                Ordering::Equal => break,
                                Ordering::Greater => break,
                            }
                        }
                    }
                }
                this.autoclose_regions.insert(
                    i,
                    AutocloseRegion {
                        selection_id,
                        range: start..end,
                        pair,
                    },
                );
            }

            let had_active_inline_completion = this.has_active_inline_completion();
            this.change_selections_inner(Some(Autoscroll::fit()), false, window, cx, |s| {
                s.select(new_selections)
            });

            if !bracket_inserted {
                if let Some(on_type_format_task) =
                    this.trigger_on_type_formatting(text.to_string(), window, cx)
                {
                    on_type_format_task.detach_and_log_err(cx);
                }
            }

            let editor_settings = EditorSettings::get_global(cx);
            if bracket_inserted
                && (editor_settings.auto_signature_help
                    || editor_settings.show_signature_help_after_edits)
            {
                this.show_signature_help(&ShowSignatureHelp, window, cx);
            }

            let trigger_in_words =
                this.show_inline_completions_in_menu(cx) || !had_active_inline_completion;
            this.trigger_completion_on_input(&text, trigger_in_words, window, cx);
            linked_editing_ranges::refresh_linked_ranges(this, window, cx);
            this.refresh_inline_completion(true, false, window, cx);
        });
    }

    fn find_possible_emoji_shortcode_at_position(
        snapshot: &MultiBufferSnapshot,
        position: Point,
    ) -> Option<String> {
        let mut chars = Vec::new();
        let mut found_colon = false;
        for char in snapshot.reversed_chars_at(position).take(100) {
            // Found a possible emoji shortcode in the middle of the buffer
            if found_colon {
                if char.is_whitespace() {
                    chars.reverse();
                    return Some(chars.iter().collect());
                }
                // If the previous character is not a whitespace, we are in the middle of a word
                // and we only want to complete the shortcode if the word is made up of other emojis
                let mut containing_word = String::new();
                for ch in snapshot
                    .reversed_chars_at(position)
                    .skip(chars.len() + 1)
                    .take(100)
                {
                    if ch.is_whitespace() {
                        break;
                    }
                    containing_word.push(ch);
                }
                let containing_word = containing_word.chars().rev().collect::<String>();
                if util::word_consists_of_emojis(containing_word.as_str()) {
                    chars.reverse();
                    return Some(chars.iter().collect());
                }
            }

            if char.is_whitespace() || !char.is_ascii() {
                return None;
            }
            if char == ':' {
                found_colon = true;
            } else {
                chars.push(char);
            }
        }
        // Found a possible emoji shortcode at the beginning of the buffer
        chars.reverse();
        Some(chars.iter().collect())
    }

    pub fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        self.transact(window, cx, |this, window, cx| {
            let (edits, selection_fixup_info): (Vec<_>, Vec<_>) = {
                let selections = this.selections.all::<usize>(cx);
                let multi_buffer = this.buffer.read(cx);
                let buffer = multi_buffer.snapshot(cx);
                selections
                    .iter()
                    .map(|selection| {
                        let start_point = selection.start.to_point(&buffer);
                        let mut indent =
                            buffer.indent_size_for_line(MultiBufferRow(start_point.row));
                        indent.len = cmp::min(indent.len, start_point.column);
                        let start = selection.start;
                        let end = selection.end;
                        let selection_is_empty = start == end;
                        let language_scope = buffer.language_scope_at(start);
                        let (comment_delimiter, insert_extra_newline) = if let Some(language) =
                            &language_scope
                        {
                            let leading_whitespace_len = buffer
                                .reversed_chars_at(start)
                                .take_while(|c| c.is_whitespace() && *c != '\n')
                                .map(|c| c.len_utf8())
                                .sum::<usize>();

                            let trailing_whitespace_len = buffer
                                .chars_at(end)
                                .take_while(|c| c.is_whitespace() && *c != '\n')
                                .map(|c| c.len_utf8())
                                .sum::<usize>();

                            let insert_extra_newline =
                                language.brackets().any(|(pair, enabled)| {
                                    let pair_start = pair.start.trim_end();
                                    let pair_end = pair.end.trim_start();

                                    enabled
                                        && pair.newline
                                        && buffer.contains_str_at(
                                            end + trailing_whitespace_len,
                                            pair_end,
                                        )
                                        && buffer.contains_str_at(
                                            (start - leading_whitespace_len)
                                                .saturating_sub(pair_start.len()),
                                            pair_start,
                                        )
                                });

                            // Comment extension on newline is allowed only for cursor selections
                            let comment_delimiter = maybe!({
                                if !selection_is_empty {
                                    return None;
                                }

                                if !multi_buffer.settings_at(0, cx).extend_comment_on_newline {
                                    return None;
                                }

                                let delimiters = language.line_comment_prefixes();
                                let max_len_of_delimiter =
                                    delimiters.iter().map(|delimiter| delimiter.len()).max()?;
                                let (snapshot, range) =
                                    buffer.buffer_line_for_row(MultiBufferRow(start_point.row))?;

                                let mut index_of_first_non_whitespace = 0;
                                let comment_candidate = snapshot
                                    .chars_for_range(range)
                                    .skip_while(|c| {
                                        let should_skip = c.is_whitespace();
                                        if should_skip {
                                            index_of_first_non_whitespace += 1;
                                        }
                                        should_skip
                                    })
                                    .take(max_len_of_delimiter)
                                    .collect::<String>();
                                let comment_prefix = delimiters.iter().find(|comment_prefix| {
                                    comment_candidate.starts_with(comment_prefix.as_ref())
                                })?;
                                let cursor_is_placed_after_comment_marker =
                                    index_of_first_non_whitespace + comment_prefix.len()
                                        <= start_point.column as usize;
                                if cursor_is_placed_after_comment_marker {
                                    Some(comment_prefix.clone())
                                } else {
                                    None
                                }
                            });
                            (comment_delimiter, insert_extra_newline)
                        } else {
                            (None, false)
                        };

                        let capacity_for_delimiter = comment_delimiter
                            .as_deref()
                            .map(str::len)
                            .unwrap_or_default();
                        let mut new_text =
                            String::with_capacity(1 + capacity_for_delimiter + indent.len as usize);
                        new_text.push('\n');
                        new_text.extend(indent.chars());
                        if let Some(delimiter) = &comment_delimiter {
                            new_text.push_str(delimiter);
                        }
                        if insert_extra_newline {
                            new_text = new_text.repeat(2);
                        }

                        let anchor = buffer.anchor_after(end);
                        let new_selection = selection.map(|_| anchor);
                        (
                            (start..end, new_text),
                            (insert_extra_newline, new_selection),
                        )
                    })
                    .unzip()
            };

            this.edit_with_autoindent(edits, cx);
            let buffer = this.buffer.read(cx).snapshot(cx);
            let new_selections = selection_fixup_info
                .into_iter()
                .map(|(extra_newline_inserted, new_selection)| {
                    let mut cursor = new_selection.end.to_point(&buffer);
                    if extra_newline_inserted {
                        cursor.row -= 1;
                        cursor.column = buffer.line_len(MultiBufferRow(cursor.row));
                    }
                    new_selection.map(|_| cursor)
                })
                .collect();

            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select(new_selections)
            });
            this.refresh_inline_completion(true, false, window, cx);
        });
    }

    pub fn newline_above(&mut self, _: &NewlineAbove, window: &mut Window, cx: &mut Context<Self>) {
        let buffer = self.buffer.read(cx);
        let snapshot = buffer.snapshot(cx);

        let mut edits = Vec::new();
        let mut rows = Vec::new();

        for (rows_inserted, selection) in self.selections.all_adjusted(cx).into_iter().enumerate() {
            let cursor = selection.head();
            let row = cursor.row;

            let start_of_line = snapshot.clip_point(Point::new(row, 0), Bias::Left);

            let newline = "\n".to_string();
            edits.push((start_of_line..start_of_line, newline));

            rows.push(row + rows_inserted as u32);
        }

        self.transact(window, cx, |editor, window, cx| {
            editor.edit(edits, cx);

            editor.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                let mut index = 0;
                s.move_cursors_with(|map, _, _| {
                    let row = rows[index];
                    index += 1;

                    let point = Point::new(row, 0);
                    let boundary = map.next_line_boundary(point).1;
                    let clipped = map.clip_point(boundary, Bias::Left);

                    (clipped, SelectionGoal::None)
                });
            });

            let mut indent_edits = Vec::new();
            let multibuffer_snapshot = editor.buffer.read(cx).snapshot(cx);
            for row in rows {
                let indents = multibuffer_snapshot.suggested_indents(row..row + 1, cx);
                for (row, indent) in indents {
                    if indent.len == 0 {
                        continue;
                    }

                    let text = match indent.kind {
                        IndentKind::Space => " ".repeat(indent.len as usize),
                        IndentKind::Tab => "\t".repeat(indent.len as usize),
                    };
                    let point = Point::new(row.0, 0);
                    indent_edits.push((point..point, text));
                }
            }
            editor.edit(indent_edits, cx);
        });
    }

    pub fn newline_below(&mut self, _: &NewlineBelow, window: &mut Window, cx: &mut Context<Self>) {
        let buffer = self.buffer.read(cx);
        let snapshot = buffer.snapshot(cx);

        let mut edits = Vec::new();
        let mut rows = Vec::new();
        let mut rows_inserted = 0;

        for selection in self.selections.all_adjusted(cx) {
            let cursor = selection.head();
            let row = cursor.row;

            let point = Point::new(row + 1, 0);
            let start_of_line = snapshot.clip_point(point, Bias::Left);

            let newline = "\n".to_string();
            edits.push((start_of_line..start_of_line, newline));

            rows_inserted += 1;
            rows.push(row + rows_inserted);
        }

        self.transact(window, cx, |editor, window, cx| {
            editor.edit(edits, cx);

            editor.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                let mut index = 0;
                s.move_cursors_with(|map, _, _| {
                    let row = rows[index];
                    index += 1;

                    let point = Point::new(row, 0);
                    let boundary = map.next_line_boundary(point).1;
                    let clipped = map.clip_point(boundary, Bias::Left);

                    (clipped, SelectionGoal::None)
                });
            });

            let mut indent_edits = Vec::new();
            let multibuffer_snapshot = editor.buffer.read(cx).snapshot(cx);
            for row in rows {
                let indents = multibuffer_snapshot.suggested_indents(row..row + 1, cx);
                for (row, indent) in indents {
                    if indent.len == 0 {
                        continue;
                    }

                    let text = match indent.kind {
                        IndentKind::Space => " ".repeat(indent.len as usize),
                        IndentKind::Tab => "\t".repeat(indent.len as usize),
                    };
                    let point = Point::new(row.0, 0);
                    indent_edits.push((point..point, text));
                }
            }
            editor.edit(indent_edits, cx);
        });
    }

    pub fn insert(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let autoindent = text.is_empty().not().then(|| AutoindentMode::Block {
            original_indent_columns: Vec::new(),
        });
        self.insert_with_autoindent_mode(text, autoindent, window, cx);
    }

    fn insert_with_autoindent_mode(
        &mut self,
        text: &str,
        autoindent_mode: Option<AutoindentMode>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }

        let text: Arc<str> = text.into();
        self.transact(window, cx, |this, window, cx| {
            let old_selections = this.selections.all_adjusted(cx);
            let selection_anchors = this.buffer.update(cx, |buffer, cx| {
                let anchors = {
                    let snapshot = buffer.read(cx);
                    old_selections
                        .iter()
                        .map(|s| {
                            let anchor = snapshot.anchor_after(s.head());
                            s.map(|_| anchor)
                        })
                        .collect::<Vec<_>>()
                };
                buffer.edit(
                    old_selections
                        .iter()
                        .map(|s| (s.start..s.end, text.clone())),
                    autoindent_mode,
                    cx,
                );
                anchors
            });

            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select_anchors(selection_anchors);
            });

            cx.notify();
        });
    }

    fn trigger_completion_on_input(
        &mut self,
        text: &str,
        trigger_in_words: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_completion_trigger(text, trigger_in_words, cx) {
            self.show_completions(
                &ShowCompletions {
                    trigger: Some(text.to_owned()).filter(|x| !x.is_empty()),
                },
                window,
                cx,
            );
        } else {
            self.hide_context_menu(window, cx);
        }
    }

    fn is_completion_trigger(
        &self,
        text: &str,
        trigger_in_words: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let position = self.selections.newest_anchor().head();
        let multibuffer = self.buffer.read(cx);
        let Some(buffer) = position
            .buffer_id
            .and_then(|buffer_id| multibuffer.buffer(buffer_id).clone())
        else {
            return false;
        };

        if let Some(completion_provider) = &self.completion_provider {
            completion_provider.is_completion_trigger(
                &buffer,
                position.text_anchor,
                text,
                trigger_in_words,
                cx,
            )
        } else {
            false
        }
    }

    /// If any empty selections is touching the start of its innermost containing autoclose
    /// region, expand it to select the brackets.
    fn select_autoclose_pair(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selections = self.selections.all::<usize>(cx);
        let buffer = self.buffer.read(cx).read(cx);
        let new_selections = self
            .selections_with_autoclose_regions(selections, &buffer)
            .map(|(mut selection, region)| {
                if !selection.is_empty() {
                    return selection;
                }

                if let Some(region) = region {
                    let mut range = region.range.to_offset(&buffer);
                    if selection.start == range.start && range.start >= region.pair.start.len() {
                        range.start -= region.pair.start.len();
                        if buffer.contains_str_at(range.start, &region.pair.start)
                            && buffer.contains_str_at(range.end, &region.pair.end)
                        {
                            range.end += region.pair.end.len();
                            selection.start = range.start;
                            selection.end = range.end;

                            return selection;
                        }
                    }
                }

                let always_treat_brackets_as_autoclosed = buffer
                    .settings_at(selection.start, cx)
                    .always_treat_brackets_as_autoclosed;

                if !always_treat_brackets_as_autoclosed {
                    return selection;
                }

                if let Some(scope) = buffer.language_scope_at(selection.start) {
                    for (pair, enabled) in scope.brackets() {
                        if !enabled || !pair.close {
                            continue;
                        }

                        if buffer.contains_str_at(selection.start, &pair.end) {
                            let pair_start_len = pair.start.len();
                            if buffer.contains_str_at(
                                selection.start.saturating_sub(pair_start_len),
                                &pair.start,
                            ) {
                                selection.start -= pair_start_len;
                                selection.end += pair.end.len();

                                return selection;
                            }
                        }
                    }
                }

                selection
            })
            .collect();

        drop(buffer);
        self.change_selections(None, window, cx, |selections| {
            selections.select(new_selections)
        });
    }

    /// Iterate the given selections, and for each one, find the smallest surrounding
    /// autoclose region. This uses the ordering of the selections and the autoclose
    /// regions to avoid repeated comparisons.
    fn selections_with_autoclose_regions<'a, D: ToOffset + Clone>(
        &'a self,
        selections: impl IntoIterator<Item = Selection<D>>,
        buffer: &'a MultiBufferSnapshot,
    ) -> impl Iterator<Item = (Selection<D>, Option<&'a AutocloseRegion>)> {
        let mut i = 0;
        let mut regions = self.autoclose_regions.as_slice();
        selections.into_iter().map(move |selection| {
            let range = selection.start.to_offset(buffer)..selection.end.to_offset(buffer);

            let mut enclosing = None;
            while let Some(pair_state) = regions.get(i) {
                if pair_state.range.end.to_offset(buffer) < range.start {
                    regions = &regions[i + 1..];
                    i = 0;
                } else if pair_state.range.start.to_offset(buffer) > range.end {
                    break;
                } else {
                    if pair_state.selection_id == selection.id {
                        enclosing = Some(pair_state);
                    }
                    i += 1;
                }
            }

            (selection, enclosing)
        })
    }

    /// Remove any autoclose regions that no longer contain their selection.
    fn invalidate_autoclose_regions(
        &mut self,
        mut selections: &[Selection<Anchor>],
        buffer: &MultiBufferSnapshot,
    ) {
        self.autoclose_regions.retain(|state| {
            let mut i = 0;
            while let Some(selection) = selections.get(i) {
                if selection.end.cmp(&state.range.start, buffer).is_lt() {
                    selections = &selections[1..];
                    continue;
                }
                if selection.start.cmp(&state.range.end, buffer).is_gt() {
                    break;
                }
                if selection.id == state.selection_id {
                    return true;
                } else {
                    i += 1;
                }
            }
            false
        });
    }

    fn completion_query(buffer: &MultiBufferSnapshot, position: impl ToOffset) -> Option<String> {
        let offset = position.to_offset(buffer);
        let (word_range, kind) = buffer.surrounding_word(offset, true);
        if offset > word_range.start && kind == Some(CharKind::Word) {
            Some(
                buffer
                    .text_for_range(word_range.start..offset)
                    .collect::<String>(),
            )
        } else {
            None
        }
    }

    pub fn toggle_inlay_hints(
        &mut self,
        _: &ToggleInlayHints,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.refresh_inlay_hints(
            InlayHintRefreshReason::Toggle(!self.inlay_hint_cache.enabled),
            cx,
        );
    }

    pub fn inlay_hints_enabled(&self) -> bool {
        self.inlay_hint_cache.enabled
    }

    fn refresh_inlay_hints(&mut self, reason: InlayHintRefreshReason, cx: &mut Context<Self>) {
        if self.semantics_provider.is_none() || self.mode != EditorMode::Full {
            return;
        }

        let reason_description = reason.description();
        let ignore_debounce = matches!(
            reason,
            InlayHintRefreshReason::SettingsChange(_)
                | InlayHintRefreshReason::Toggle(_)
                | InlayHintRefreshReason::ExcerptsRemoved(_)
        );
        let (invalidate_cache, required_languages) = match reason {
            InlayHintRefreshReason::Toggle(enabled) => {
                self.inlay_hint_cache.enabled = enabled;
                if enabled {
                    (InvalidationStrategy::RefreshRequested, None)
                } else {
                    self.inlay_hint_cache.clear();
                    self.splice_inlays(
                        &self
                            .visible_inlay_hints(cx)
                            .iter()
                            .map(|inlay| inlay.id)
                            .collect::<Vec<InlayId>>(),
                        Vec::new(),
                        cx,
                    );
                    return;
                }
            }
            InlayHintRefreshReason::SettingsChange(new_settings) => {
                match self.inlay_hint_cache.update_settings(
                    &self.buffer,
                    new_settings,
                    self.visible_inlay_hints(cx),
                    cx,
                ) {
                    ControlFlow::Break(Some(InlaySplice {
                        to_remove,
                        to_insert,
                    })) => {
                        self.splice_inlays(&to_remove, to_insert, cx);
                        return;
                    }
                    ControlFlow::Break(None) => return,
                    ControlFlow::Continue(()) => (InvalidationStrategy::RefreshRequested, None),
                }
            }
            InlayHintRefreshReason::ExcerptsRemoved(excerpts_removed) => {
                if let Some(InlaySplice {
                    to_remove,
                    to_insert,
                }) = self.inlay_hint_cache.remove_excerpts(excerpts_removed)
                {
                    self.splice_inlays(&to_remove, to_insert, cx);
                }
                return;
            }
            InlayHintRefreshReason::NewLinesShown => (InvalidationStrategy::None, None),
            InlayHintRefreshReason::BufferEdited(buffer_languages) => {
                (InvalidationStrategy::BufferEdited, Some(buffer_languages))
            }
            InlayHintRefreshReason::RefreshRequested => {
                (InvalidationStrategy::RefreshRequested, None)
            }
        };

        if let Some(InlaySplice {
            to_remove,
            to_insert,
        }) = self.inlay_hint_cache.spawn_hint_refresh(
            reason_description,
            self.excerpts_for_inlay_hints_query(required_languages.as_ref(), cx),
            invalidate_cache,
            ignore_debounce,
            cx,
        ) {
            self.splice_inlays(&to_remove, to_insert, cx);
        }
    }

    fn visible_inlay_hints(&self, cx: &Context<Editor>) -> Vec<Inlay> {
        self.display_map
            .read(cx)
            .current_inlays()
            .filter(move |inlay| matches!(inlay.id, InlayId::Hint(_)))
            .cloned()
            .collect()
    }

    pub fn excerpts_for_inlay_hints_query(
        &self,
        restrict_to_languages: Option<&HashSet<Arc<Language>>>,
        cx: &mut Context<Editor>,
    ) -> HashMap<ExcerptId, (Entity<Buffer>, clock::Global, Range<usize>)> {
        let Some(project) = self.project.as_ref() else {
            return HashMap::default();
        };
        let project = project.read(cx);
        let multi_buffer = self.buffer().read(cx);
        let multi_buffer_snapshot = multi_buffer.snapshot(cx);
        let multi_buffer_visible_start = self
            .scroll_manager
            .anchor()
            .anchor
            .to_point(&multi_buffer_snapshot);
        let multi_buffer_visible_end = multi_buffer_snapshot.clip_point(
            multi_buffer_visible_start
                + Point::new(self.visible_line_count().unwrap_or(0.).ceil() as u32, 0),
            Bias::Left,
        );
        let multi_buffer_visible_range = multi_buffer_visible_start..multi_buffer_visible_end;
        multi_buffer_snapshot
            .range_to_buffer_ranges(multi_buffer_visible_range)
            .into_iter()
            .filter(|(_, excerpt_visible_range, _)| !excerpt_visible_range.is_empty())
            .filter_map(|(buffer, excerpt_visible_range, excerpt_id)| {
                let buffer_file = project::File::from_dyn(buffer.file())?;
                let buffer_worktree = project.worktree_for_id(buffer_file.worktree_id(cx), cx)?;
                let worktree_entry = buffer_worktree
                    .read(cx)
                    .entry_for_id(buffer_file.project_entry_id(cx)?)?;
                if worktree_entry.is_ignored {
                    return None;
                }

                let language = buffer.language()?;
                if let Some(restrict_to_languages) = restrict_to_languages {
                    if !restrict_to_languages.contains(language) {
                        return None;
                    }
                }
                Some((
                    excerpt_id,
                    (
                        multi_buffer.buffer(buffer.remote_id()).unwrap(),
                        buffer.version().clone(),
                        excerpt_visible_range,
                    ),
                ))
            })
            .collect()
    }

    pub fn text_layout_details(&self, window: &mut Window) -> TextLayoutDetails {
        TextLayoutDetails {
            text_system: window.text_system().clone(),
            editor_style: self.style.clone().unwrap(),
            rem_size: window.rem_size(),
            scroll_anchor: self.scroll_manager.anchor(),
            visible_rows: self.visible_line_count(),
            vertical_scroll_margin: self.scroll_manager.vertical_scroll_margin,
        }
    }

    pub fn splice_inlays(
        &self,
        to_remove: &[InlayId],
        to_insert: Vec<Inlay>,
        cx: &mut Context<Self>,
    ) {
        self.display_map.update(cx, |display_map, cx| {
            display_map.splice_inlays(to_remove, to_insert, cx)
        });
        cx.notify();
    }

    fn trigger_on_type_formatting(
        &self,
        input: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        if input.len() != 1 {
            return None;
        }

        let project = self.project.as_ref()?;
        let position = self.selections.newest_anchor().head();
        let (buffer, buffer_position) = self
            .buffer
            .read(cx)
            .text_anchor_for_position(position, cx)?;

        let settings = language_settings::language_settings(
            buffer
                .read(cx)
                .language_at(buffer_position)
                .map(|l| l.name()),
            buffer.read(cx).file(),
            cx,
        );
        if !settings.use_on_type_format {
            return None;
        }

        // OnTypeFormatting returns a list of edits, no need to pass them between Zed instances,
        // hence we do LSP request & edit on host side only — add formats to host's history.
        let push_to_lsp_host_history = true;
        // If this is not the host, append its history with new edits.
        let push_to_client_history = project.read(cx).is_via_collab();

        let on_type_formatting = project.update(cx, |project, cx| {
            project.on_type_format(
                buffer.clone(),
                buffer_position,
                input,
                push_to_lsp_host_history,
                cx,
            )
        });
        Some(cx.spawn_in(window, |editor, mut cx| async move {
            if let Some(transaction) = on_type_formatting.await? {
                if push_to_client_history {
                    buffer
                        .update(&mut cx, |buffer, _| {
                            buffer.push_transaction(transaction, Instant::now());
                        })
                        .ok();
                }
                editor.update(&mut cx, |editor, cx| {
                    editor.refresh_document_highlights(cx);
                })?;
            }
            Ok(())
        }))
    }

    pub fn show_completions(
        &mut self,
        options: &ShowCompletions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_rename.is_some() {
            return;
        }

        let Some(provider) = self.completion_provider.as_ref() else {
            return;
        };

        if !self.snippet_stack.is_empty() && self.context_menu.borrow().as_ref().is_some() {
            return;
        }

        let position = self.selections.newest_anchor().head();
        if position.diff_base_anchor.is_some() {
            return;
        }
        let (buffer, buffer_position) =
            if let Some(output) = self.buffer.read(cx).text_anchor_for_position(position, cx) {
                output
            } else {
                return;
            };
        let show_completion_documentation = buffer
            .read(cx)
            .snapshot()
            .settings_at(buffer_position, cx)
            .show_completion_documentation;

        let query = Self::completion_query(&self.buffer.read(cx).read(cx), position);

        let trigger_kind = match &options.trigger {
            Some(trigger) if buffer.read(cx).completion_triggers().contains(trigger) => {
                CompletionTriggerKind::TRIGGER_CHARACTER
            }
            _ => CompletionTriggerKind::INVOKED,
        };
        let completion_context = CompletionContext {
            trigger_character: options.trigger.as_ref().and_then(|trigger| {
                if trigger_kind == CompletionTriggerKind::TRIGGER_CHARACTER {
                    Some(String::from(trigger))
                } else {
                    None
                }
            }),
            trigger_kind,
        };
        let completions =
            provider.completions(&buffer, buffer_position, completion_context, window, cx);
        let sort_completions = provider.sort_completions();

        let id = post_inc(&mut self.next_completion_id);
        let task = cx.spawn_in(window, |editor, mut cx| {
            async move {
                editor.update(&mut cx, |this, _| {
                    this.completion_tasks.retain(|(task_id, _)| *task_id >= id);
                })?;
                let completions = completions.await.log_err();
                let menu = if let Some(completions) = completions {
                    let mut menu = CompletionsMenu::new(
                        id,
                        sort_completions,
                        show_completion_documentation,
                        position,
                        buffer.clone(),
                        completions.into(),
                    );

                    menu.filter(query.as_deref(), cx.background_executor().clone())
                        .await;

                    menu.visible().then_some(menu)
                } else {
                    None
                };

                editor.update_in(&mut cx, |editor, window, cx| {
                    match editor.context_menu.borrow().as_ref() {
                        None => {}
                        Some(CodeContextMenu::Completions(prev_menu)) => {
                            if prev_menu.id > id {
                                return;
                            }
                        }
                        _ => return,
                    }

                    if editor.focus_handle.is_focused(window) && menu.is_some() {
                        let mut menu = menu.unwrap();
                        menu.resolve_visible_completions(editor.completion_provider.as_deref(), cx);

                        *editor.context_menu.borrow_mut() =
                            Some(CodeContextMenu::Completions(menu));

                        if editor.show_inline_completions_in_menu(cx) {
                            editor.update_visible_inline_completion(window, cx);
                        } else {
                            editor.discard_inline_completion(false, cx);
                        }

                        cx.notify();
                    } else if editor.completion_tasks.len() <= 1 {
                        // If there are no more completion tasks and the last menu was
                        // empty, we should hide it.
                        let was_hidden = editor.hide_context_menu(window, cx).is_none();
                        // If it was already hidden and we don't show inline
                        // completions in the menu, we should also show the
                        // inline-completion when available.
                        if was_hidden && editor.show_inline_completions_in_menu(cx) {
                            editor.update_visible_inline_completion(window, cx);
                        }
                    }
                })?;

                Ok::<_, anyhow::Error>(())
            }
            .log_err()
        });

        self.completion_tasks.push((id, task));
    }

    pub fn confirm_completion(
        &mut self,
        action: &ConfirmCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        self.do_completion(action.item_ix, CompletionIntent::Complete, window, cx)
    }

    pub fn compose_completion(
        &mut self,
        action: &ComposeCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        self.do_completion(action.item_ix, CompletionIntent::Compose, window, cx)
    }

    fn do_completion(
        &mut self,
        item_ix: Option<usize>,
        intent: CompletionIntent,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) -> Option<Task<std::result::Result<(), anyhow::Error>>> {
        use language::ToOffset as _;

        let completions_menu =
            if let CodeContextMenu::Completions(menu) = self.hide_context_menu(window, cx)? {
                menu
            } else {
                return None;
            };

        let entries = completions_menu.entries.borrow();
        let mat = entries.get(item_ix.unwrap_or(completions_menu.selected_item))?;
        if self.show_inline_completions_in_menu(cx) {
            self.discard_inline_completion(true, cx);
        }
        let candidate_id = mat.candidate_id;
        drop(entries);

        let buffer_handle = completions_menu.buffer;
        let completion = completions_menu
            .completions
            .borrow()
            .get(candidate_id)?
            .clone();
        cx.stop_propagation();

        let snippet;
        let text;

        if completion.is_snippet() {
            snippet = Some(Snippet::parse(&completion.new_text).log_err()?);
            text = snippet.as_ref().unwrap().text.clone();
        } else {
            snippet = None;
            text = completion.new_text.clone();
        };
        let selections = self.selections.all::<usize>(cx);
        let buffer = buffer_handle.read(cx);
        let old_range = completion.old_range.to_offset(buffer);
        let old_text = buffer.text_for_range(old_range.clone()).collect::<String>();

        let newest_selection = self.selections.newest_anchor();
        if newest_selection.start.buffer_id != Some(buffer_handle.read(cx).remote_id()) {
            return None;
        }

        let lookbehind = newest_selection
            .start
            .text_anchor
            .to_offset(buffer)
            .saturating_sub(old_range.start);
        let lookahead = old_range
            .end
            .saturating_sub(newest_selection.end.text_anchor.to_offset(buffer));
        let mut common_prefix_len = old_text
            .bytes()
            .zip(text.bytes())
            .take_while(|(a, b)| a == b)
            .count();

        let snapshot = self.buffer.read(cx).snapshot(cx);
        let mut range_to_replace: Option<Range<isize>> = None;
        let mut ranges = Vec::new();
        let mut linked_edits = HashMap::<_, Vec<_>>::default();
        for selection in &selections {
            if snapshot.contains_str_at(selection.start.saturating_sub(lookbehind), &old_text) {
                let start = selection.start.saturating_sub(lookbehind);
                let end = selection.end + lookahead;
                if selection.id == newest_selection.id {
                    range_to_replace = Some(
                        ((start + common_prefix_len) as isize - selection.start as isize)
                            ..(end as isize - selection.start as isize),
                    );
                }
                ranges.push(start + common_prefix_len..end);
            } else {
                common_prefix_len = 0;
                ranges.clear();
                ranges.extend(selections.iter().map(|s| {
                    if s.id == newest_selection.id {
                        range_to_replace = Some(
                            old_range.start.to_offset_utf16(&snapshot).0 as isize
                                - selection.start as isize
                                ..old_range.end.to_offset_utf16(&snapshot).0 as isize
                                    - selection.start as isize,
                        );
                        old_range.clone()
                    } else {
                        s.start..s.end
                    }
                }));
                break;
            }
            if !self.linked_edit_ranges.is_empty() {
                let start_anchor = snapshot.anchor_before(selection.head());
                let end_anchor = snapshot.anchor_after(selection.tail());
                if let Some(ranges) = self
                    .linked_editing_ranges_for(start_anchor.text_anchor..end_anchor.text_anchor, cx)
                {
                    for (buffer, edits) in ranges {
                        linked_edits.entry(buffer.clone()).or_default().extend(
                            edits
                                .into_iter()
                                .map(|range| (range, text[common_prefix_len..].to_owned())),
                        );
                    }
                }
            }
        }
        let text = &text[common_prefix_len..];

        cx.emit(EditorEvent::InputHandled {
            utf16_range_to_replace: range_to_replace,
            text: text.into(),
        });

        self.transact(window, cx, |this, window, cx| {
            if let Some(mut snippet) = snippet {
                snippet.text = text.to_string();
                for tabstop in snippet
                    .tabstops
                    .iter_mut()
                    .flat_map(|tabstop| tabstop.ranges.iter_mut())
                {
                    tabstop.start -= common_prefix_len as isize;
                    tabstop.end -= common_prefix_len as isize;
                }

                this.insert_snippet(&ranges, snippet, window, cx).log_err();
            } else {
                this.buffer.update(cx, |buffer, cx| {
                    buffer.edit(
                        ranges.iter().map(|range| (range.clone(), text)),
                        this.autoindent_mode.clone(),
                        cx,
                    );
                });
            }
            for (buffer, edits) in linked_edits {
                buffer.update(cx, |buffer, cx| {
                    let snapshot = buffer.snapshot();
                    let edits = edits
                        .into_iter()
                        .map(|(range, text)| {
                            use text::ToPoint as TP;
                            let end_point = TP::to_point(&range.end, &snapshot);
                            let start_point = TP::to_point(&range.start, &snapshot);
                            (start_point..end_point, text)
                        })
                        .sorted_by_key(|(range, _)| range.start)
                        .collect::<Vec<_>>();
                    buffer.edit(edits, None, cx);
                })
            }

            this.refresh_inline_completion(true, false, window, cx);
        });

        let show_new_completions_on_confirm = completion
            .confirm
            .as_ref()
            .map_or(false, |confirm| confirm(intent, window, cx));
        if show_new_completions_on_confirm {
            self.show_completions(&ShowCompletions { trigger: None }, window, cx);
        }

        let provider = self.completion_provider.as_ref()?;
        drop(completion);
        let apply_edits = provider.apply_additional_edits_for_completion(
            buffer_handle,
            completions_menu.completions.clone(),
            candidate_id,
            true,
            cx,
        );

        let editor_settings = EditorSettings::get_global(cx);
        if editor_settings.show_signature_help_after_edits || editor_settings.auto_signature_help {
            // After the code completion is finished, users often want to know what signatures are needed.
            // so we should automatically call signature_help
            self.show_signature_help(&ShowSignatureHelp, window, cx);
        }

        Some(cx.foreground_executor().spawn(async move {
            apply_edits.await?;
            Ok(())
        }))
    }

    pub fn toggle_code_actions(
        &mut self,
        action: &ToggleCodeActions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut context_menu = self.context_menu.borrow_mut();
        if let Some(CodeContextMenu::CodeActions(code_actions)) = context_menu.as_ref() {
            if code_actions.deployed_from_indicator == action.deployed_from_indicator {
                // Toggle if we're selecting the same one
                *context_menu = None;
                cx.notify();
                return;
            } else {
                // Otherwise, clear it and start a new one
                *context_menu = None;
                cx.notify();
            }
        }
        drop(context_menu);
        let snapshot = self.snapshot(window, cx);
        let deployed_from_indicator = action.deployed_from_indicator;
        let mut task = self.code_actions_task.take();
        let action = action.clone();
        cx.spawn_in(window, |editor, mut cx| async move {
            while let Some(prev_task) = task {
                prev_task.await.log_err();
                task = editor.update(&mut cx, |this, _| this.code_actions_task.take())?;
            }

            let spawned_test_task = editor.update_in(&mut cx, |editor, window, cx| {
                if editor.focus_handle.is_focused(window) {
                    let multibuffer_point = action
                        .deployed_from_indicator
                        .map(|row| DisplayPoint::new(row, 0).to_point(&snapshot))
                        .unwrap_or_else(|| editor.selections.newest::<Point>(cx).head());
                    let (buffer, buffer_row) = snapshot
                        .buffer_snapshot
                        .buffer_line_for_row(MultiBufferRow(multibuffer_point.row))
                        .and_then(|(buffer_snapshot, range)| {
                            editor
                                .buffer
                                .read(cx)
                                .buffer(buffer_snapshot.remote_id())
                                .map(|buffer| (buffer, range.start.row))
                        })?;
                    let (_, code_actions) = editor
                        .available_code_actions
                        .clone()
                        .and_then(|(location, code_actions)| {
                            let snapshot = location.buffer.read(cx).snapshot();
                            let point_range = location.range.to_point(&snapshot);
                            let point_range = point_range.start.row..=point_range.end.row;
                            if point_range.contains(&buffer_row) {
                                Some((location, code_actions))
                            } else {
                                None
                            }
                        })
                        .unzip();
                    let buffer_id = buffer.read(cx).remote_id();
                    let tasks = editor
                        .tasks
                        .get(&(buffer_id, buffer_row))
                        .map(|t| Arc::new(t.to_owned()));
                    if tasks.is_none() && code_actions.is_none() {
                        return None;
                    }

                    editor.completion_tasks.clear();
                    editor.discard_inline_completion(false, cx);
                    let task_context =
                        tasks
                            .as_ref()
                            .zip(editor.project.clone())
                            .map(|(tasks, project)| {
                                Self::build_tasks_context(&project, &buffer, buffer_row, tasks, cx)
                            });

                    Some(cx.spawn_in(window, |editor, mut cx| async move {
                        let task_context = match task_context {
                            Some(task_context) => task_context.await,
                            None => None,
                        };
                        let resolved_tasks =
                            tasks.zip(task_context).map(|(tasks, task_context)| {
                                Rc::new(ResolvedTasks {
                                    templates: tasks.resolve(&task_context).collect(),
                                    position: snapshot.buffer_snapshot.anchor_before(Point::new(
                                        multibuffer_point.row,
                                        tasks.column,
                                    )),
                                })
                            });
                        let spawn_straight_away = resolved_tasks
                            .as_ref()
                            .map_or(false, |tasks| tasks.templates.len() == 1)
                            && code_actions
                                .as_ref()
                                .map_or(true, |actions| actions.is_empty());
                        if let Ok(task) = editor.update_in(&mut cx, |editor, window, cx| {
                            *editor.context_menu.borrow_mut() =
                                Some(CodeContextMenu::CodeActions(CodeActionsMenu {
                                    buffer,
                                    actions: CodeActionContents {
                                        tasks: resolved_tasks,
                                        actions: code_actions,
                                    },
                                    selected_item: Default::default(),
                                    scroll_handle: UniformListScrollHandle::default(),
                                    deployed_from_indicator,
                                }));
                            if spawn_straight_away {
                                if let Some(task) = editor.confirm_code_action(
                                    &ConfirmCodeAction { item_ix: Some(0) },
                                    window,
                                    cx,
                                ) {
                                    cx.notify();
                                    return task;
                                }
                            }
                            cx.notify();
                            Task::ready(Ok(()))
                        }) {
                            task.await
                        } else {
                            Ok(())
                        }
                    }))
                } else {
                    Some(Task::ready(Ok(())))
                }
            })?;
            if let Some(task) = spawned_test_task {
                task.await?;
            }

            Ok::<_, anyhow::Error>(())
        })
        .detach_and_log_err(cx);
    }

    pub fn confirm_code_action(
        &mut self,
        action: &ConfirmCodeAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        let actions_menu =
            if let CodeContextMenu::CodeActions(menu) = self.hide_context_menu(window, cx)? {
                menu
            } else {
                return None;
            };
        let action_ix = action.item_ix.unwrap_or(actions_menu.selected_item);
        let action = actions_menu.actions.get(action_ix)?;
        let title = action.label();
        let buffer = actions_menu.buffer;
        let workspace = self.workspace()?;

        match action {
            CodeActionsItem::Task(task_source_kind, resolved_task) => {
                workspace.update(cx, |workspace, cx| {
                    workspace::tasks::schedule_resolved_task(
                        workspace,
                        task_source_kind,
                        resolved_task,
                        false,
                        cx,
                    );

                    Some(Task::ready(Ok(())))
                })
            }
            CodeActionsItem::CodeAction {
                excerpt_id,
                action,
                provider,
            } => {
                let apply_code_action =
                    provider.apply_code_action(buffer, action, excerpt_id, true, window, cx);
                let workspace = workspace.downgrade();
                Some(cx.spawn_in(window, |editor, cx| async move {
                    let project_transaction = apply_code_action.await?;
                    Self::open_project_transaction(
                        &editor,
                        workspace,
                        project_transaction,
                        title,
                        cx,
                    )
                    .await
                }))
            }
        }
    }

    pub async fn open_project_transaction(
        this: &WeakEntity<Editor>,
        workspace: WeakEntity<Workspace>,
        transaction: ProjectTransaction,
        title: String,
        mut cx: AsyncWindowContext,
    ) -> Result<()> {
        let mut entries = transaction.0.into_iter().collect::<Vec<_>>();
        cx.update(|_, cx| {
            entries.sort_unstable_by_key(|(buffer, _)| {
                buffer.read(cx).file().map(|f| f.path().clone())
            });
        })?;

        // If the project transaction's edits are all contained within this editor, then
        // avoid opening a new editor to display them.

        if let Some((buffer, transaction)) = entries.first() {
            if entries.len() == 1 {
                let excerpt = this.update(&mut cx, |editor, cx| {
                    editor
                        .buffer()
                        .read(cx)
                        .excerpt_containing(editor.selections.newest_anchor().head(), cx)
                })?;
                if let Some((_, excerpted_buffer, excerpt_range)) = excerpt {
                    if excerpted_buffer == *buffer {
                        let all_edits_within_excerpt = buffer.read_with(&cx, |buffer, _| {
                            let excerpt_range = excerpt_range.to_offset(buffer);
                            buffer
                                .edited_ranges_for_transaction::<usize>(transaction)
                                .all(|range| {
                                    excerpt_range.start <= range.start
                                        && excerpt_range.end >= range.end
                                })
                        })?;

                        if all_edits_within_excerpt {
                            return Ok(());
                        }
                    }
                }
            }
        } else {
            return Ok(());
        }

        let mut ranges_to_highlight = Vec::new();
        let excerpt_buffer = cx.new(|cx| {
            let mut multibuffer = MultiBuffer::new(Capability::ReadWrite).with_title(title);
            for (buffer_handle, transaction) in &entries {
                let buffer = buffer_handle.read(cx);
                ranges_to_highlight.extend(
                    multibuffer.push_excerpts_with_context_lines(
                        buffer_handle.clone(),
                        buffer
                            .edited_ranges_for_transaction::<usize>(transaction)
                            .collect(),
                        DEFAULT_MULTIBUFFER_CONTEXT,
                        cx,
                    ),
                );
            }
            multibuffer.push_transaction(entries.iter().map(|(b, t)| (b, t)), cx);
            multibuffer
        })?;

        workspace.update_in(&mut cx, |workspace, window, cx| {
            let project = workspace.project().clone();
            let editor = cx
                .new(|cx| Editor::for_multibuffer(excerpt_buffer, Some(project), true, window, cx));
            workspace.add_item_to_active_pane(Box::new(editor.clone()), None, true, window, cx);
            editor.update(cx, |editor, cx| {
                editor.highlight_background::<Self>(
                    &ranges_to_highlight,
                    |theme| theme.editor_highlighted_line_background,
                    cx,
                );
            });
        })?;

        Ok(())
    }

    pub fn clear_code_action_providers(&mut self) {
        self.code_action_providers.clear();
        self.available_code_actions.take();
    }

    pub fn add_code_action_provider(
        &mut self,
        provider: Rc<dyn CodeActionProvider>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .code_action_providers
            .iter()
            .any(|existing_provider| existing_provider.id() == provider.id())
        {
            return;
        }

        self.code_action_providers.push(provider);
        self.refresh_code_actions(window, cx);
    }

    pub fn remove_code_action_provider(
        &mut self,
        id: Arc<str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.code_action_providers
            .retain(|provider| provider.id() != id);
        self.refresh_code_actions(window, cx);
    }

    fn refresh_code_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<()> {
        let buffer = self.buffer.read(cx);
        let newest_selection = self.selections.newest_anchor().clone();
        if newest_selection.head().diff_base_anchor.is_some() {
            return None;
        }
        let (start_buffer, start) = buffer.text_anchor_for_position(newest_selection.start, cx)?;
        let (end_buffer, end) = buffer.text_anchor_for_position(newest_selection.end, cx)?;
        if start_buffer != end_buffer {
            return None;
        }

        self.code_actions_task = Some(cx.spawn_in(window, |this, mut cx| async move {
            cx.background_executor()
                .timer(CODE_ACTIONS_DEBOUNCE_TIMEOUT)
                .await;

            let (providers, tasks) = this.update_in(&mut cx, |this, window, cx| {
                let providers = this.code_action_providers.clone();
                let tasks = this
                    .code_action_providers
                    .iter()
                    .map(|provider| provider.code_actions(&start_buffer, start..end, window, cx))
                    .collect::<Vec<_>>();
                (providers, tasks)
            })?;

            let mut actions = Vec::new();
            for (provider, provider_actions) in
                providers.into_iter().zip(future::join_all(tasks).await)
            {
                if let Some(provider_actions) = provider_actions.log_err() {
                    actions.extend(provider_actions.into_iter().map(|action| {
                        AvailableCodeAction {
                            excerpt_id: newest_selection.start.excerpt_id,
                            action,
                            provider: provider.clone(),
                        }
                    }));
                }
            }

            this.update(&mut cx, |this, cx| {
                this.available_code_actions = if actions.is_empty() {
                    None
                } else {
                    Some((
                        Location {
                            buffer: start_buffer,
                            range: start..end,
                        },
                        actions.into(),
                    ))
                };
                cx.notify();
            })
        }));
        None
    }

    fn start_inline_blame_timer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(delay) = ProjectSettings::get_global(cx).git.inline_blame_delay() {
            self.show_git_blame_inline = false;

            self.show_git_blame_inline_delay_task =
                Some(cx.spawn_in(window, |this, mut cx| async move {
                    cx.background_executor().timer(delay).await;

                    this.update(&mut cx, |this, cx| {
                        this.show_git_blame_inline = true;
                        cx.notify();
                    })
                    .log_err();
                }));
        }
    }

    fn refresh_document_highlights(&mut self, cx: &mut Context<Self>) -> Option<()> {
        if self.pending_rename.is_some() {
            return None;
        }

        let provider = self.semantics_provider.clone()?;
        let buffer = self.buffer.read(cx);
        let newest_selection = self.selections.newest_anchor().clone();
        let cursor_position = newest_selection.head();
        let (cursor_buffer, cursor_buffer_position) =
            buffer.text_anchor_for_position(cursor_position, cx)?;
        let (tail_buffer, _) = buffer.text_anchor_for_position(newest_selection.tail(), cx)?;
        if cursor_buffer != tail_buffer {
            return None;
        }
        let debounce = EditorSettings::get_global(cx).lsp_highlight_debounce;
        self.document_highlights_task = Some(cx.spawn(|this, mut cx| async move {
            cx.background_executor()
                .timer(Duration::from_millis(debounce))
                .await;

            let highlights = if let Some(highlights) = cx
                .update(|cx| {
                    provider.document_highlights(&cursor_buffer, cursor_buffer_position, cx)
                })
                .ok()
                .flatten()
            {
                highlights.await.log_err()
            } else {
                None
            };

            if let Some(highlights) = highlights {
                this.update(&mut cx, |this, cx| {
                    if this.pending_rename.is_some() {
                        return;
                    }

                    let buffer_id = cursor_position.buffer_id;
                    let buffer = this.buffer.read(cx);
                    if !buffer
                        .text_anchor_for_position(cursor_position, cx)
                        .map_or(false, |(buffer, _)| buffer == cursor_buffer)
                    {
                        return;
                    }

                    let cursor_buffer_snapshot = cursor_buffer.read(cx);
                    let mut write_ranges = Vec::new();
                    let mut read_ranges = Vec::new();
                    for highlight in highlights {
                        for (excerpt_id, excerpt_range) in
                            buffer.excerpts_for_buffer(cursor_buffer.read(cx).remote_id(), cx)
                        {
                            let start = highlight
                                .range
                                .start
                                .max(&excerpt_range.context.start, cursor_buffer_snapshot);
                            let end = highlight
                                .range
                                .end
                                .min(&excerpt_range.context.end, cursor_buffer_snapshot);
                            if start.cmp(&end, cursor_buffer_snapshot).is_ge() {
                                continue;
                            }

                            let range = Anchor {
                                buffer_id,
                                excerpt_id,
                                text_anchor: start,
                                diff_base_anchor: None,
                            }..Anchor {
                                buffer_id,
                                excerpt_id,
                                text_anchor: end,
                                diff_base_anchor: None,
                            };
                            if highlight.kind == lsp::DocumentHighlightKind::WRITE {
                                write_ranges.push(range);
                            } else {
                                read_ranges.push(range);
                            }
                        }
                    }

                    this.highlight_background::<DocumentHighlightRead>(
                        &read_ranges,
                        |theme| theme.editor_document_highlight_read_background,
                        cx,
                    );
                    this.highlight_background::<DocumentHighlightWrite>(
                        &write_ranges,
                        |theme| theme.editor_document_highlight_write_background,
                        cx,
                    );
                    cx.notify();
                })
                .log_err();
            }
        }));
        None
    }

    pub fn refresh_inline_completion(
        &mut self,
        debounce: bool,
        user_requested: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<()> {
        let provider = self.inline_completion_provider()?;
        let cursor = self.selections.newest_anchor().head();
        let (buffer, cursor_buffer_position) =
            self.buffer.read(cx).text_anchor_for_position(cursor, cx)?;

        if !self.inline_completions_enabled_in_buffer(&buffer, cursor_buffer_position, cx) {
            self.discard_inline_completion(false, cx);
            return None;
        }

        if !user_requested
            && (!self.show_inline_completions
                || !self.should_show_inline_completions_in_buffer(
                    &buffer,
                    cursor_buffer_position,
                    cx,
                )
                || !self.is_focused(window)
                || buffer.read(cx).is_empty())
        {
            self.discard_inline_completion(false, cx);
            return None;
        }

        self.update_visible_inline_completion(window, cx);
        provider.refresh(
            self.project.clone(),
            buffer,
            cursor_buffer_position,
            debounce,
            cx,
        );
        Some(())
    }

    pub fn should_show_inline_completions(&self, cx: &App) -> bool {
        let cursor = self.selections.newest_anchor().head();
        if let Some((buffer, cursor_position)) =
            self.buffer.read(cx).text_anchor_for_position(cursor, cx)
        {
            self.should_show_inline_completions_in_buffer(&buffer, cursor_position, cx)
        } else {
            false
        }
    }

    fn inline_completion_requires_modifier(&self, cx: &App) -> bool {
        let cursor = self.selections.newest_anchor().head();

        self.buffer
            .read(cx)
            .text_anchor_for_position(cursor, cx)
            .map(|(buffer, _)| {
                all_language_settings(buffer.read(cx).file(), cx).inline_completions_preview_mode()
                    == InlineCompletionPreviewMode::WhenHoldingModifier
            })
            .unwrap_or(false)
    }

    fn should_show_inline_completions_in_buffer(
        &self,
        buffer: &Entity<Buffer>,
        buffer_position: language::Anchor,
        cx: &App,
    ) -> bool {
        if !self.snippet_stack.is_empty() {
            return false;
        }

        if self.inline_completions_disabled_in_scope(buffer, buffer_position, cx) {
            return false;
        }

        if let Some(show_inline_completions) = self.show_inline_completions_override {
            show_inline_completions
        } else {
            let buffer = buffer.read(cx);
            self.mode == EditorMode::Full
                && language_settings(
                    buffer.language_at(buffer_position).map(|l| l.name()),
                    buffer.file(),
                    cx,
                )
                .show_inline_completions
        }
    }

    pub fn inline_completions_enabled(&self, cx: &App) -> bool {
        let cursor = self.selections.newest_anchor().head();
        if let Some((buffer, cursor_position)) =
            self.buffer.read(cx).text_anchor_for_position(cursor, cx)
        {
            self.inline_completions_enabled_in_buffer(&buffer, cursor_position, cx)
        } else {
            false
        }
    }

    fn inline_completions_enabled_in_buffer(
        &self,
        buffer: &Entity<Buffer>,
        buffer_position: language::Anchor,
        cx: &App,
    ) -> bool {
        maybe!({
            let provider = self.inline_completion_provider()?;
            if !provider.is_enabled(&buffer, buffer_position, cx) {
                return Some(false);
            }
            let buffer = buffer.read(cx);
            let Some(file) = buffer.file() else {
                return Some(true);
            };
            let settings = all_language_settings(Some(file), cx);
            Some(settings.inline_completions_enabled_for_path(file.path()))
        })
        .unwrap_or(false)
    }

    fn cycle_inline_completion(
        &mut self,
        direction: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<()> {
        let provider = self.inline_completion_provider()?;
        let cursor = self.selections.newest_anchor().head();
        let (buffer, cursor_buffer_position) =
            self.buffer.read(cx).text_anchor_for_position(cursor, cx)?;
        if !self.show_inline_completions
            || !self.should_show_inline_completions_in_buffer(&buffer, cursor_buffer_position, cx)
        {
            return None;
        }

        provider.cycle(buffer, cursor_buffer_position, direction, cx);
        self.update_visible_inline_completion(window, cx);

        Some(())
    }

    pub fn show_inline_completion(
        &mut self,
        _: &ShowInlineCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.has_active_inline_completion() {
            self.refresh_inline_completion(false, true, window, cx);
            return;
        }

        self.update_visible_inline_completion(window, cx);
    }

    pub fn display_cursor_names(
        &mut self,
        _: &DisplayCursorNames,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_cursor_names(window, cx);
    }

    fn show_cursor_names(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_cursor_names = true;
        cx.notify();
        cx.spawn_in(window, |this, mut cx| async move {
            cx.background_executor().timer(CURSORS_VISIBLE_FOR).await;
            this.update(&mut cx, |this, cx| {
                this.show_cursor_names = false;
                cx.notify()
            })
            .ok()
        })
        .detach();
    }

    pub fn next_inline_completion(
        &mut self,
        _: &NextInlineCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.has_active_inline_completion() {
            self.cycle_inline_completion(Direction::Next, window, cx);
        } else {
            let is_copilot_disabled = self
                .refresh_inline_completion(false, true, window, cx)
                .is_none();
            if is_copilot_disabled {
                cx.propagate();
            }
        }
    }

    pub fn previous_inline_completion(
        &mut self,
        _: &PreviousInlineCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.has_active_inline_completion() {
            self.cycle_inline_completion(Direction::Prev, window, cx);
        } else {
            let is_copilot_disabled = self
                .refresh_inline_completion(false, true, window, cx)
                .is_none();
            if is_copilot_disabled {
                cx.propagate();
            }
        }
    }

    pub fn accept_inline_completion(
        &mut self,
        _: &AcceptInlineCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let buffer = self.buffer.read(cx);
        let snapshot = buffer.snapshot(cx);
        let selection = self.selections.newest_adjusted(cx);
        let cursor = selection.head();
        let current_indent = snapshot.indent_size_for_line(MultiBufferRow(cursor.row));
        let suggested_indents = snapshot.suggested_indents([cursor.row], cx);
        if let Some(suggested_indent) = suggested_indents.get(&MultiBufferRow(cursor.row)).copied()
        {
            if cursor.column < suggested_indent.len
                && cursor.column <= current_indent.len
                && current_indent.len <= suggested_indent.len
            {
                self.tab(&Default::default(), window, cx);
                return;
            }
        }

        if self.show_inline_completions_in_menu(cx) {
            self.hide_context_menu(window, cx);
        }

        let Some(active_inline_completion) = self.active_inline_completion.as_ref() else {
            return;
        };

        self.report_inline_completion_event(true, cx);

        match &active_inline_completion.completion {
            InlineCompletion::Move { target, .. } => {
                let target = *target;
                self.change_selections(Some(Autoscroll::newest()), window, cx, |selections| {
                    selections.select_anchor_ranges([target..target]);
                });
            }
            InlineCompletion::Edit { edits, .. } => {
                if let Some(provider) = self.inline_completion_provider() {
                    provider.accept(cx);
                }

                let snapshot = self.buffer.read(cx).snapshot(cx);
                let last_edit_end = edits.last().unwrap().0.end.bias_right(&snapshot);

                self.buffer.update(cx, |buffer, cx| {
                    buffer.edit(edits.iter().cloned(), None, cx)
                });

                self.change_selections(None, window, cx, |s| {
                    s.select_anchor_ranges([last_edit_end..last_edit_end])
                });

                self.update_visible_inline_completion(window, cx);
                if self.active_inline_completion.is_none() {
                    self.refresh_inline_completion(true, true, window, cx);
                }

                cx.notify();
            }
        }
    }

    pub fn accept_partial_inline_completion(
        &mut self,
        _: &AcceptPartialInlineCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(active_inline_completion) = self.active_inline_completion.as_ref() else {
            return;
        };
        if self.selections.count() != 1 {
            return;
        }

        self.report_inline_completion_event(true, cx);

        match &active_inline_completion.completion {
            InlineCompletion::Move { target, .. } => {
                let target = *target;
                self.change_selections(Some(Autoscroll::newest()), window, cx, |selections| {
                    selections.select_anchor_ranges([target..target]);
                });
            }
            InlineCompletion::Edit { edits, .. } => {
                // Find an insertion that starts at the cursor position.
                let snapshot = self.buffer.read(cx).snapshot(cx);
                let cursor_offset = self.selections.newest::<usize>(cx).head();
                let insertion = edits.iter().find_map(|(range, text)| {
                    let range = range.to_offset(&snapshot);
                    if range.is_empty() && range.start == cursor_offset {
                        Some(text)
                    } else {
                        None
                    }
                });

                if let Some(text) = insertion {
                    let mut partial_completion = text
                        .chars()
                        .by_ref()
                        .take_while(|c| c.is_alphabetic())
                        .collect::<String>();
                    if partial_completion.is_empty() {
                        partial_completion = text
                            .chars()
                            .by_ref()
                            .take_while(|c| c.is_whitespace() || !c.is_alphabetic())
                            .collect::<String>();
                    }

                    cx.emit(EditorEvent::InputHandled {
                        utf16_range_to_replace: None,
                        text: partial_completion.clone().into(),
                    });

                    self.insert_with_autoindent_mode(&partial_completion, None, window, cx);

                    self.refresh_inline_completion(true, true, window, cx);
                    cx.notify();
                } else {
                    self.accept_inline_completion(&Default::default(), window, cx);
                }
            }
        }
    }

    fn discard_inline_completion(
        &mut self,
        should_report_inline_completion_event: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        if should_report_inline_completion_event {
            self.report_inline_completion_event(false, cx);
        }

        if let Some(provider) = self.inline_completion_provider() {
            provider.discard(cx);
        }

        self.take_active_inline_completion(cx)
    }

    fn report_inline_completion_event(&self, accepted: bool, cx: &App) {
        let Some(provider) = self.inline_completion_provider() else {
            return;
        };

        let Some((_, buffer, _)) = self
            .buffer
            .read(cx)
            .excerpt_containing(self.selections.newest_anchor().head(), cx)
        else {
            return;
        };

        let extension = buffer
            .read(cx)
            .file()
            .and_then(|file| Some(file.path().extension()?.to_string_lossy().to_string()));

        let event_type = match accepted {
            true => "Edit Prediction Accepted",
            false => "Edit Prediction Discarded",
        };
        telemetry::event!(
            event_type,
            provider = provider.name(),
            suggestion_accepted = accepted,
            file_extension = extension,
        );
    }

    pub fn has_active_inline_completion(&self) -> bool {
        self.active_inline_completion.is_some()
    }

    fn take_active_inline_completion(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(active_inline_completion) = self.active_inline_completion.take() else {
            return false;
        };

        self.splice_inlays(&active_inline_completion.inlay_ids, Default::default(), cx);
        self.clear_highlights::<InlineCompletionHighlight>(cx);
        self.stale_inline_completion_in_menu = Some(active_inline_completion);
        true
    }

    /// Returns true when we're displaying the inline completion popover below the cursor
    /// like we are not previewing and the LSP autocomplete menu is visible
    /// or we are in `when_holding_modifier` mode.
    pub fn inline_completion_visible_in_cursor_popover(
        &self,
        has_completion: bool,
        cx: &App,
    ) -> bool {
        if self.previewing_inline_completion
            || !self.show_inline_completions_in_menu(cx)
            || !self.should_show_inline_completions(cx)
        {
            return false;
        }

        if self.has_visible_completions_menu() {
            return true;
        }

        has_completion && self.inline_completion_requires_modifier(cx)
    }

    fn update_inline_completion_preview(
        &mut self,
        modifiers: &Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.show_inline_completions_in_menu(cx) {
            return;
        }

        self.previewing_inline_completion = modifiers.alt;
        self.update_visible_inline_completion(window, cx);
        cx.notify();
    }

    fn update_visible_inline_completion(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<()> {
        let selection = self.selections.newest_anchor();
        let cursor = selection.head();
        let multibuffer = self.buffer.read(cx).snapshot(cx);
        let offset_selection = selection.map(|endpoint| endpoint.to_offset(&multibuffer));
        let excerpt_id = cursor.excerpt_id;

        let show_in_menu = self.show_inline_completions_in_menu(cx);
        let completions_menu_has_precedence = !show_in_menu
            && (self.context_menu.borrow().is_some()
                || (!self.completion_tasks.is_empty() && !self.has_active_inline_completion()));
        if completions_menu_has_precedence
            || !offset_selection.is_empty()
            || !self.show_inline_completions
            || self
                .active_inline_completion
                .as_ref()
                .map_or(false, |completion| {
                    let invalidation_range = completion.invalidation_range.to_offset(&multibuffer);
                    let invalidation_range = invalidation_range.start..=invalidation_range.end;
                    !invalidation_range.contains(&offset_selection.head())
                })
        {
            self.discard_inline_completion(false, cx);
            return None;
        }

        self.take_active_inline_completion(cx);
        let provider = self.inline_completion_provider()?;

        let (buffer, cursor_buffer_position) =
            self.buffer.read(cx).text_anchor_for_position(cursor, cx)?;

        let inline_completion = provider.suggest(&buffer, cursor_buffer_position, cx)?;
        let edits = inline_completion
            .edits
            .into_iter()
            .flat_map(|(range, new_text)| {
                let start = multibuffer.anchor_in_excerpt(excerpt_id, range.start)?;
                let end = multibuffer.anchor_in_excerpt(excerpt_id, range.end)?;
                Some((start..end, new_text))
            })
            .collect::<Vec<_>>();
        if edits.is_empty() {
            return None;
        }

        let first_edit_start = edits.first().unwrap().0.start;
        let first_edit_start_point = first_edit_start.to_point(&multibuffer);
        let edit_start_row = first_edit_start_point.row.saturating_sub(2);

        let last_edit_end = edits.last().unwrap().0.end;
        let last_edit_end_point = last_edit_end.to_point(&multibuffer);
        let edit_end_row = cmp::min(multibuffer.max_point().row, last_edit_end_point.row + 2);

        let cursor_row = cursor.to_point(&multibuffer).row;

        let snapshot = multibuffer.buffer_for_excerpt(excerpt_id).cloned()?;

        let mut inlay_ids = Vec::new();
        let invalidation_row_range;
        let move_invalidation_row_range = if cursor_row < edit_start_row {
            Some(cursor_row..edit_end_row)
        } else if cursor_row > edit_end_row {
            Some(edit_start_row..cursor_row)
        } else {
            None
        };
        let completion = if let Some(move_invalidation_row_range) = move_invalidation_row_range {
            invalidation_row_range = move_invalidation_row_range;
            let target = first_edit_start;
            let target_point = text::ToPoint::to_point(&target.text_anchor, &snapshot);
            // TODO: Base this off of TreeSitter or word boundaries?
            let target_excerpt_begin = snapshot.anchor_before(snapshot.clip_point(
                Point::new(target_point.row, target_point.column.saturating_sub(20)),
                Bias::Left,
            ));
            let target_excerpt_end = snapshot.anchor_after(snapshot.clip_point(
                Point::new(target_point.row, target_point.column + 20),
                Bias::Right,
            ));
            let range_around_target = target_excerpt_begin..target_excerpt_end;
            InlineCompletion::Move {
                target,
                range_around_target,
                snapshot,
            }
        } else {
            if !self.inline_completion_visible_in_cursor_popover(true, cx) {
                if edits
                    .iter()
                    .all(|(range, _)| range.to_offset(&multibuffer).is_empty())
                {
                    let mut inlays = Vec::new();
                    for (range, new_text) in &edits {
                        let inlay = Inlay::inline_completion(
                            post_inc(&mut self.next_inlay_id),
                            range.start,
                            new_text.as_str(),
                        );
                        inlay_ids.push(inlay.id);
                        inlays.push(inlay);
                    }

                    self.splice_inlays(&[], inlays, cx);
                } else {
                    let background_color = cx.theme().status().deleted_background;
                    self.highlight_text::<InlineCompletionHighlight>(
                        edits.iter().map(|(range, _)| range.clone()).collect(),
                        HighlightStyle {
                            background_color: Some(background_color),
                            ..Default::default()
                        },
                        cx,
                    );
                }
            }

            invalidation_row_range = edit_start_row..edit_end_row;

            let display_mode = if all_edits_insertions_or_deletions(&edits, &multibuffer) {
                if provider.show_tab_accept_marker() {
                    EditDisplayMode::TabAccept
                } else {
                    EditDisplayMode::Inline
                }
            } else {
                EditDisplayMode::DiffPopover
            };

            InlineCompletion::Edit {
                edits,
                edit_preview: inline_completion.edit_preview,
                display_mode,
                snapshot,
            }
        };

        let invalidation_range = multibuffer
            .anchor_before(Point::new(invalidation_row_range.start, 0))
            ..multibuffer.anchor_after(Point::new(
                invalidation_row_range.end,
                multibuffer.line_len(MultiBufferRow(invalidation_row_range.end)),
            ));

        self.stale_inline_completion_in_menu = None;
        self.active_inline_completion = Some(InlineCompletionState {
            inlay_ids,
            completion,
            invalidation_range,
        });

        cx.notify();

        Some(())
    }

    pub fn inline_completion_provider(&self) -> Option<Arc<dyn InlineCompletionProviderHandle>> {
        Some(self.inline_completion_provider.as_ref()?.provider.clone())
    }

    fn show_inline_completions_in_menu(&self, cx: &App) -> bool {
        let by_provider = matches!(
            self.menu_inline_completions_policy,
            MenuInlineCompletionsPolicy::ByProvider
        );

        by_provider
            && EditorSettings::get_global(cx).show_inline_completions_in_menu
            && self
                .inline_completion_provider()
                .map_or(false, |provider| provider.show_completions_in_menu())
    }

    fn render_code_actions_indicator(
        &self,
        _style: &EditorStyle,
        row: DisplayRow,
        is_active: bool,
        cx: &mut Context<Self>,
    ) -> Option<IconButton> {
        if self.available_code_actions.is_some() {
            Some(
                IconButton::new("code_actions_indicator", ui::IconName::Bolt)
                    .shape(ui::IconButtonShape::Square)
                    .icon_size(IconSize::XSmall)
                    .icon_color(Color::Muted)
                    .toggle_state(is_active)
                    .tooltip({
                        let focus_handle = self.focus_handle.clone();
                        move |window, cx| {
                            Tooltip::for_action_in(
                                "Toggle Code Actions",
                                &ToggleCodeActions {
                                    deployed_from_indicator: None,
                                },
                                &focus_handle,
                                window,
                                cx,
                            )
                        }
                    })
                    .on_click(cx.listener(move |editor, _e, window, cx| {
                        window.focus(&editor.focus_handle(cx));
                        editor.toggle_code_actions(
                            &ToggleCodeActions {
                                deployed_from_indicator: Some(row),
                            },
                            window,
                            cx,
                        );
                    })),
            )
        } else {
            None
        }
    }

    fn clear_tasks(&mut self) {
        self.tasks.clear()
    }

    fn insert_tasks(&mut self, key: (BufferId, BufferRow), value: RunnableTasks) {
        if self.tasks.insert(key, value).is_some() {
            // This case should hopefully be rare, but just in case...
            log::error!("multiple different run targets found on a single line, only the last target will be rendered")
        }
    }

    fn build_tasks_context(
        project: &Entity<Project>,
        buffer: &Entity<Buffer>,
        buffer_row: u32,
        tasks: &Arc<RunnableTasks>,
        cx: &mut Context<Self>,
    ) -> Task<Option<task::TaskContext>> {
        let position = Point::new(buffer_row, tasks.column);
        let range_start = buffer.read(cx).anchor_at(position, Bias::Right);
        let location = Location {
            buffer: buffer.clone(),
            range: range_start..range_start,
        };
        // Fill in the environmental variables from the tree-sitter captures
        let mut captured_task_variables = TaskVariables::default();
        for (capture_name, value) in tasks.extra_variables.clone() {
            captured_task_variables.insert(
                task::VariableName::Custom(capture_name.into()),
                value.clone(),
            );
        }
        project.update(cx, |project, cx| {
            project.task_store().update(cx, |task_store, cx| {
                task_store.task_context_for_location(captured_task_variables, location, cx)
            })
        })
    }

    pub fn spawn_nearest_task(
        &mut self,
        action: &SpawnNearestTask,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((workspace, _)) = self.workspace.clone() else {
            return;
        };
        let Some(project) = self.project.clone() else {
            return;
        };

        // Try to find a closest, enclosing node using tree-sitter that has a
        // task
        let Some((buffer, buffer_row, tasks)) = self
            .find_enclosing_node_task(cx)
            // Or find the task that's closest in row-distance.
            .or_else(|| self.find_closest_task(cx))
        else {
            return;
        };

        let reveal_strategy = action.reveal;
        let task_context = Self::build_tasks_context(&project, &buffer, buffer_row, &tasks, cx);
        cx.spawn_in(window, |_, mut cx| async move {
            let context = task_context.await?;
            let (task_source_kind, mut resolved_task) = tasks.resolve(&context).next()?;

            let resolved = resolved_task.resolved.as_mut()?;
            resolved.reveal = reveal_strategy;

            workspace
                .update(&mut cx, |workspace, cx| {
                    workspace::tasks::schedule_resolved_task(
                        workspace,
                        task_source_kind,
                        resolved_task,
                        false,
                        cx,
                    );
                })
                .ok()
        })
        .detach();
    }

    fn find_closest_task(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<(Entity<Buffer>, u32, Arc<RunnableTasks>)> {
        let cursor_row = self.selections.newest_adjusted(cx).head().row;

        let ((buffer_id, row), tasks) = self
            .tasks
            .iter()
            .min_by_key(|((_, row), _)| cursor_row.abs_diff(*row))?;

        let buffer = self.buffer.read(cx).buffer(*buffer_id)?;
        let tasks = Arc::new(tasks.to_owned());
        Some((buffer, *row, tasks))
    }

    fn find_enclosing_node_task(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<(Entity<Buffer>, u32, Arc<RunnableTasks>)> {
        let snapshot = self.buffer.read(cx).snapshot(cx);
        let offset = self.selections.newest::<usize>(cx).head();
        let excerpt = snapshot.excerpt_containing(offset..offset)?;
        let buffer_id = excerpt.buffer().remote_id();

        let layer = excerpt.buffer().syntax_layer_at(offset)?;
        let mut cursor = layer.node().walk();

        while cursor.goto_first_child_for_byte(offset).is_some() {
            if cursor.node().end_byte() == offset {
                cursor.goto_next_sibling();
            }
        }

        // Ascend to the smallest ancestor that contains the range and has a task.
        loop {
            let node = cursor.node();
            let node_range = node.byte_range();
            let symbol_start_row = excerpt.buffer().offset_to_point(node.start_byte()).row;

            // Check if this node contains our offset
            if node_range.start <= offset && node_range.end >= offset {
                // If it contains offset, check for task
                if let Some(tasks) = self.tasks.get(&(buffer_id, symbol_start_row)) {
                    let buffer = self.buffer.read(cx).buffer(buffer_id)?;
                    return Some((buffer, symbol_start_row, Arc::new(tasks.to_owned())));
                }
            }

            if !cursor.goto_parent() {
                break;
            }
        }
        None
    }

    fn render_run_indicator(
        &self,
        _style: &EditorStyle,
        is_active: bool,
        row: DisplayRow,
        cx: &mut Context<Self>,
    ) -> IconButton {
        IconButton::new(("run_indicator", row.0 as usize), ui::IconName::Play)
            .shape(ui::IconButtonShape::Square)
            .icon_size(IconSize::XSmall)
            .icon_color(Color::Muted)
            .toggle_state(is_active)
            .on_click(cx.listener(move |editor, _e, window, cx| {
                window.focus(&editor.focus_handle(cx));
                editor.toggle_code_actions(
                    &ToggleCodeActions {
                        deployed_from_indicator: Some(row),
                    },
                    window,
                    cx,
                );
            }))
    }

    pub fn context_menu_visible(&self) -> bool {
        !self.previewing_inline_completion
            && self
                .context_menu
                .borrow()
                .as_ref()
                .map_or(false, |menu| menu.visible())
    }

    fn context_menu_origin(&self) -> Option<ContextMenuOrigin> {
        self.context_menu
            .borrow()
            .as_ref()
            .map(|menu| menu.origin())
    }

    fn edit_prediction_cursor_popover_height(&self) -> Pixels {
        px(30.)
    }

    fn current_user_player_color(&self, cx: &mut App) -> PlayerColor {
        if self.read_only(cx) {
            cx.theme().players().read_only()
        } else {
            self.style.as_ref().unwrap().local_player
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_edit_prediction_cursor_popover(
        &self,
        min_width: Pixels,
        max_width: Pixels,
        cursor_point: Point,
        style: &EditorStyle,
        accept_keystroke: &gpui::Keystroke,
        window: &Window,
        cx: &mut Context<Editor>,
    ) -> Option<AnyElement> {
        let provider = self.inline_completion_provider.as_ref()?;

        if provider.provider.needs_terms_acceptance(cx) {
            return Some(
                h_flex()
                    .h(self.edit_prediction_cursor_popover_height())
                    .min_w(min_width)
                    .flex_1()
                    .px_2()
                    .gap_3()
                    .elevation_2(cx)
                    .hover(|style| style.bg(cx.theme().colors().element_hover))
                    .id("accept-terms")
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(|this, _event, window, cx| {
                        cx.stop_propagation();
                        this.report_editor_event("Edit Prediction Provider ToS Clicked", None, cx);
                        window.dispatch_action(
                            zed_actions::OpenZedPredictOnboarding.boxed_clone(),
                            cx,
                        );
                    }))
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(Icon::new(IconName::ZedPredict))
                            .child(Label::new("Accept Terms of Service"))
                            .child(div().w_full())
                            .child(
                                Icon::new(IconName::ArrowUpRight)
                                    .color(Color::Muted)
                                    .size(IconSize::Small),
                            )
                            .into_any_element(),
                    )
                    .into_any(),
            );
        }

        let is_refreshing = provider.provider.is_refreshing(cx);

        fn pending_completion_container() -> Div {
            h_flex()
                .h_full()
                .flex_1()
                .gap_2()
                .child(Icon::new(IconName::ZedPredict))
        }

        let completion = match &self.active_inline_completion {
            Some(completion) => self.render_edit_prediction_cursor_popover_preview(
                completion,
                cursor_point,
                style,
                window,
                cx,
            )?,

            None if is_refreshing => match &self.stale_inline_completion_in_menu {
                Some(stale_completion) => self.render_edit_prediction_cursor_popover_preview(
                    stale_completion,
                    cursor_point,
                    style,
                    window,
                    cx,
                )?,

                None => {
                    pending_completion_container().child(Label::new("...").size(LabelSize::Small))
                }
            },

            None => pending_completion_container().child(Label::new("No Prediction")),
        };

        let buffer_font = theme::ThemeSettings::get_global(cx).buffer_font.clone();
        let completion = completion.font(buffer_font.clone());

        let completion = if is_refreshing {
            completion
                .with_animation(
                    "loading-completion",
                    Animation::new(Duration::from_secs(2))
                        .repeat()
                        .with_easing(pulsating_between(0.4, 0.8)),
                    |label, delta| label.opacity(delta),
                )
                .into_any_element()
        } else {
            completion.into_any_element()
        };

        let has_completion = self.active_inline_completion.is_some();

        Some(
            h_flex()
                .h(self.edit_prediction_cursor_popover_height())
                .min_w(min_width)
                .max_w(max_width)
                .flex_1()
                .px_2()
                .elevation_2(cx)
                .child(completion)
                .child(ui::Divider::vertical())
                .child(
                    h_flex()
                        .h_full()
                        .gap_1()
                        .pl_2()
                        .child(h_flex().font(buffer_font.clone()).gap_1().children(
                            ui::render_modifiers(
                                &accept_keystroke.modifiers,
                                PlatformStyle::platform(),
                                Some(if !has_completion {
                                    Color::Muted
                                } else {
                                    Color::Default
                                }),
                                None,
                                true,
                            ),
                        ))
                        .child(Label::new("Preview").into_any_element())
                        .opacity(if has_completion { 1.0 } else { 0.4 }),
                )
                .into_any(),
        )
    }

    fn render_edit_prediction_cursor_popover_preview(
        &self,
        completion: &InlineCompletionState,
        cursor_point: Point,
        style: &EditorStyle,
        window: &Window,
        cx: &mut Context<Editor>,
    ) -> Option<Div> {
        use text::ToPoint as _;

        fn render_relative_row_jump(
            prefix: impl Into<String>,
            current_row: u32,
            target_row: u32,
        ) -> Div {
            let (row_diff, arrow) = if target_row < current_row {
                (current_row - target_row, IconName::ArrowUp)
            } else {
                (target_row - current_row, IconName::ArrowDown)
            };

            h_flex()
                .child(
                    Label::new(format!("{}{}", prefix.into(), row_diff))
                        .color(Color::Muted)
                        .size(LabelSize::Small),
                )
                .child(Icon::new(arrow).color(Color::Muted).size(IconSize::Small))
        }

        match &completion.completion {
            InlineCompletion::Edit {
                edits,
                edit_preview,
                snapshot,
                display_mode: _,
            } => {
                let first_edit_row = edits.first()?.0.start.text_anchor.to_point(&snapshot).row;

                let highlighted_edits = crate::inline_completion_edit_text(
                    &snapshot,
                    &edits,
                    edit_preview.as_ref()?,
                    true,
                    cx,
                );

                let len_total = highlighted_edits.text.len();
                let first_line = &highlighted_edits.text
                    [..highlighted_edits.text.find('\n').unwrap_or(len_total)];
                let first_line_len = first_line.len();

                let first_highlight_start = highlighted_edits
                    .highlights
                    .first()
                    .map_or(0, |(range, _)| range.start);
                let drop_prefix_len = first_line
                    .char_indices()
                    .find(|(_, c)| !c.is_whitespace())
                    .map_or(first_highlight_start, |(ix, _)| {
                        ix.min(first_highlight_start)
                    });

                let preview_text = &first_line[drop_prefix_len..];
                let preview_len = preview_text.len();
                let highlights = highlighted_edits
                    .highlights
                    .into_iter()
                    .take_until(|(range, _)| range.start > first_line_len)
                    .map(|(range, style)| {
                        (
                            range.start - drop_prefix_len
                                ..(range.end - drop_prefix_len).min(preview_len),
                            style,
                        )
                    });

                let styled_text = gpui::StyledText::new(SharedString::new(preview_text))
                    .with_highlights(&style.text, highlights);

                let preview = h_flex()
                    .gap_1()
                    .min_w_16()
                    .child(styled_text)
                    .when(len_total > first_line_len, |parent| parent.child("…"));

                let left = if first_edit_row != cursor_point.row {
                    render_relative_row_jump("", cursor_point.row, first_edit_row)
                        .into_any_element()
                } else {
                    Icon::new(IconName::ZedPredict).into_any_element()
                };

                Some(
                    h_flex()
                        .h_full()
                        .flex_1()
                        .gap_2()
                        .pr_1()
                        .overflow_x_hidden()
                        .child(left)
                        .child(preview),
                )
            }

            InlineCompletion::Move {
                target,
                range_around_target,
                snapshot,
            } => {
                let highlighted_text = snapshot.highlighted_text_for_range(
                    range_around_target.clone(),
                    None,
                    &style.syntax,
                );
                let base = h_flex().gap_3().flex_1().child(render_relative_row_jump(
                    "Jump ",
                    cursor_point.row,
                    target.text_anchor.to_point(&snapshot).row,
                ));

                if highlighted_text.text.is_empty() {
                    return Some(base);
                }

                let cursor_color = self.current_user_player_color(cx).cursor;

                let start_point = range_around_target.start.to_point(&snapshot);
                let end_point = range_around_target.end.to_point(&snapshot);
                let target_point = target.text_anchor.to_point(&snapshot);

                let styled_text = highlighted_text.to_styled_text(&style.text);
                let text_len = highlighted_text.text.len();

                let cursor_relative_position = window
                    .text_system()
                    .layout_line(
                        highlighted_text.text,
                        style.text.font_size.to_pixels(window.rem_size()),
                        // We don't need to include highlights
                        // because we are only using this for the cursor position
                        &[TextRun {
                            len: text_len,
                            font: style.text.font(),
                            color: style.text.color,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        }],
                    )
                    .log_err()
                    .map(|line| {
                        line.x_for_index(
                            target_point.column.saturating_sub(start_point.column) as usize
                        )
                    });

                let fade_before = start_point.column > 0;
                let fade_after = end_point.column < snapshot.line_len(end_point.row);

                let background = cx.theme().colors().elevated_surface_background;

                let preview = h_flex()
                    .relative()
                    .child(styled_text)
                    .when(fade_before, |parent| {
                        parent.child(div().absolute().top_0().left_0().w_4().h_full().bg(
                            linear_gradient(
                                90.,
                                linear_color_stop(background, 0.),
                                linear_color_stop(background.opacity(0.), 1.),
                            ),
                        ))
                    })
                    .when(fade_after, |parent| {
                        parent.child(div().absolute().top_0().right_0().w_4().h_full().bg(
                            linear_gradient(
                                -90.,
                                linear_color_stop(background, 0.),
                                linear_color_stop(background.opacity(0.), 1.),
                            ),
                        ))
                    })
                    .when_some(cursor_relative_position, |parent, position| {
                        parent.child(
                            div()
                                .w(px(2.))
                                .h_full()
                                .bg(cursor_color)
                                .absolute()
                                .top_0()
                                .left(position),
                        )
                    });

                Some(base.child(preview))
            }
        }
    }

    fn render_context_menu(
        &self,
        style: &EditorStyle,
        max_height_in_lines: u32,
        y_flipped: bool,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) -> Option<AnyElement> {
        let menu = self.context_menu.borrow();
        let menu = menu.as_ref()?;
        if !menu.visible() {
            return None;
        };
        Some(menu.render(style, max_height_in_lines, y_flipped, window, cx))
    }

    fn render_context_menu_aside(
        &self,
        style: &EditorStyle,
        max_size: Size<Pixels>,
        cx: &mut Context<Editor>,
    ) -> Option<AnyElement> {
        self.context_menu.borrow().as_ref().and_then(|menu| {
            if menu.visible() {
                menu.render_aside(
                    style,
                    max_size,
                    self.workspace.as_ref().map(|(w, _)| w.clone()),
                    cx,
                )
            } else {
                None
            }
        })
    }

    fn hide_context_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<CodeContextMenu> {
        cx.notify();
        self.completion_tasks.clear();
        let context_menu = self.context_menu.borrow_mut().take();
        self.stale_inline_completion_in_menu.take();
        self.update_visible_inline_completion(window, cx);
        context_menu
    }

    fn show_snippet_choices(
        &mut self,
        choices: &Vec<String>,
        selection: Range<Anchor>,
        cx: &mut Context<Self>,
    ) {
        if selection.start.buffer_id.is_none() {
            return;
        }
        let buffer_id = selection.start.buffer_id.unwrap();
        let buffer = self.buffer().read(cx).buffer(buffer_id);
        let id = post_inc(&mut self.next_completion_id);

        if let Some(buffer) = buffer {
            *self.context_menu.borrow_mut() = Some(CodeContextMenu::Completions(
                CompletionsMenu::new_snippet_choices(id, true, choices, selection, buffer),
            ));
        }
    }

    pub fn insert_snippet(
        &mut self,
        insertion_ranges: &[Range<usize>],
        snippet: Snippet,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        struct Tabstop<T> {
            is_end_tabstop: bool,
            ranges: Vec<Range<T>>,
            choices: Option<Vec<String>>,
        }

        let tabstops = self.buffer.update(cx, |buffer, cx| {
            let snippet_text: Arc<str> = snippet.text.clone().into();
            buffer.edit(
                insertion_ranges
                    .iter()
                    .cloned()
                    .map(|range| (range, snippet_text.clone())),
                Some(AutoindentMode::EachLine),
                cx,
            );

            let snapshot = &*buffer.read(cx);
            let snippet = &snippet;
            snippet
                .tabstops
                .iter()
                .map(|tabstop| {
                    let is_end_tabstop = tabstop.ranges.first().map_or(false, |tabstop| {
                        tabstop.is_empty() && tabstop.start == snippet.text.len() as isize
                    });
                    let mut tabstop_ranges = tabstop
                        .ranges
                        .iter()
                        .flat_map(|tabstop_range| {
                            let mut delta = 0_isize;
                            insertion_ranges.iter().map(move |insertion_range| {
                                let insertion_start = insertion_range.start as isize + delta;
                                delta +=
                                    snippet.text.len() as isize - insertion_range.len() as isize;

                                let start = ((insertion_start + tabstop_range.start) as usize)
                                    .min(snapshot.len());
                                let end = ((insertion_start + tabstop_range.end) as usize)
                                    .min(snapshot.len());
                                snapshot.anchor_before(start)..snapshot.anchor_after(end)
                            })
                        })
                        .collect::<Vec<_>>();
                    tabstop_ranges.sort_unstable_by(|a, b| a.start.cmp(&b.start, snapshot));

                    Tabstop {
                        is_end_tabstop,
                        ranges: tabstop_ranges,
                        choices: tabstop.choices.clone(),
                    }
                })
                .collect::<Vec<_>>()
        });
        if let Some(tabstop) = tabstops.first() {
            self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select_ranges(tabstop.ranges.iter().cloned());
            });

            if let Some(choices) = &tabstop.choices {
                if let Some(selection) = tabstop.ranges.first() {
                    self.show_snippet_choices(choices, selection.clone(), cx)
                }
            }

            // If we're already at the last tabstop and it's at the end of the snippet,
            // we're done, we don't need to keep the state around.
            if !tabstop.is_end_tabstop {
                let choices = tabstops
                    .iter()
                    .map(|tabstop| tabstop.choices.clone())
                    .collect();

                let ranges = tabstops
                    .into_iter()
                    .map(|tabstop| tabstop.ranges)
                    .collect::<Vec<_>>();

                self.snippet_stack.push(SnippetState {
                    active_index: 0,
                    ranges,
                    choices,
                });
            }

            // Check whether the just-entered snippet ends with an auto-closable bracket.
            if self.autoclose_regions.is_empty() {
                let snapshot = self.buffer.read(cx).snapshot(cx);
                for selection in &mut self.selections.all::<Point>(cx) {
                    let selection_head = selection.head();
                    let Some(scope) = snapshot.language_scope_at(selection_head) else {
                        continue;
                    };

                    let mut bracket_pair = None;
                    let next_chars = snapshot.chars_at(selection_head).collect::<String>();
                    let prev_chars = snapshot
                        .reversed_chars_at(selection_head)
                        .collect::<String>();
                    for (pair, enabled) in scope.brackets() {
                        if enabled
                            && pair.close
                            && prev_chars.starts_with(pair.start.as_str())
                            && next_chars.starts_with(pair.end.as_str())
                        {
                            bracket_pair = Some(pair.clone());
                            break;
                        }
                    }
                    if let Some(pair) = bracket_pair {
                        let start = snapshot.anchor_after(selection_head);
                        let end = snapshot.anchor_after(selection_head);
                        self.autoclose_regions.push(AutocloseRegion {
                            selection_id: selection.id,
                            range: start..end,
                            pair,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    pub fn move_to_next_snippet_tabstop(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.move_to_snippet_tabstop(Bias::Right, window, cx)
    }

    pub fn move_to_prev_snippet_tabstop(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.move_to_snippet_tabstop(Bias::Left, window, cx)
    }

    pub fn move_to_snippet_tabstop(
        &mut self,
        bias: Bias,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(mut snippet) = self.snippet_stack.pop() {
            match bias {
                Bias::Left => {
                    if snippet.active_index > 0 {
                        snippet.active_index -= 1;
                    } else {
                        self.snippet_stack.push(snippet);
                        return false;
                    }
                }
                Bias::Right => {
                    if snippet.active_index + 1 < snippet.ranges.len() {
                        snippet.active_index += 1;
                    } else {
                        self.snippet_stack.push(snippet);
                        return false;
                    }
                }
            }
            if let Some(current_ranges) = snippet.ranges.get(snippet.active_index) {
                self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                    s.select_anchor_ranges(current_ranges.iter().cloned())
                });

                if let Some(choices) = &snippet.choices[snippet.active_index] {
                    if let Some(selection) = current_ranges.first() {
                        self.show_snippet_choices(&choices, selection.clone(), cx);
                    }
                }

                // If snippet state is not at the last tabstop, push it back on the stack
                if snippet.active_index + 1 < snippet.ranges.len() {
                    self.snippet_stack.push(snippet);
                }
                return true;
            }
        }

        false
    }

    pub fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.transact(window, cx, |this, window, cx| {
            this.select_all(&SelectAll, window, cx);
            this.insert("", window, cx);
        });
    }

    pub fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        self.transact(window, cx, |this, window, cx| {
            this.select_autoclose_pair(window, cx);
            let mut linked_ranges = HashMap::<_, Vec<_>>::default();
            if !this.linked_edit_ranges.is_empty() {
                let selections = this.selections.all::<MultiBufferPoint>(cx);
                let snapshot = this.buffer.read(cx).snapshot(cx);

                for selection in selections.iter() {
                    let selection_start = snapshot.anchor_before(selection.start).text_anchor;
                    let selection_end = snapshot.anchor_after(selection.end).text_anchor;
                    if selection_start.buffer_id != selection_end.buffer_id {
                        continue;
                    }
                    if let Some(ranges) =
                        this.linked_editing_ranges_for(selection_start..selection_end, cx)
                    {
                        for (buffer, entries) in ranges {
                            linked_ranges.entry(buffer).or_default().extend(entries);
                        }
                    }
                }
            }

            let mut selections = this.selections.all::<MultiBufferPoint>(cx);
            if !this.selections.line_mode {
                let display_map = this.display_map.update(cx, |map, cx| map.snapshot(cx));
                for selection in &mut selections {
                    if selection.is_empty() {
                        let old_head = selection.head();
                        let mut new_head =
                            movement::left(&display_map, old_head.to_display_point(&display_map))
                                .to_point(&display_map);
                        if let Some((buffer, line_buffer_range)) = display_map
                            .buffer_snapshot
                            .buffer_line_for_row(MultiBufferRow(old_head.row))
                        {
                            let indent_size =
                                buffer.indent_size_for_line(line_buffer_range.start.row);
                            let indent_len = match indent_size.kind {
                                IndentKind::Space => {
                                    buffer.settings_at(line_buffer_range.start, cx).tab_size
                                }
                                IndentKind::Tab => NonZeroU32::new(1).unwrap(),
                            };
                            if old_head.column <= indent_size.len && old_head.column > 0 {
                                let indent_len = indent_len.get();
                                new_head = cmp::min(
                                    new_head,
                                    MultiBufferPoint::new(
                                        old_head.row,
                                        ((old_head.column - 1) / indent_len) * indent_len,
                                    ),
                                );
                            }
                        }

                        selection.set_head(new_head, SelectionGoal::None);
                    }
                }
            }

            this.signature_help_state.set_backspace_pressed(true);
            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select(selections)
            });
            this.insert("", window, cx);
            let empty_str: Arc<str> = Arc::from("");
            for (buffer, edits) in linked_ranges {
                let snapshot = buffer.read(cx).snapshot();
                use text::ToPoint as TP;

                let edits = edits
                    .into_iter()
                    .map(|range| {
                        let end_point = TP::to_point(&range.end, &snapshot);
                        let mut start_point = TP::to_point(&range.start, &snapshot);

                        if end_point == start_point {
                            let offset = text::ToOffset::to_offset(&range.start, &snapshot)
                                .saturating_sub(1);
                            start_point =
                                snapshot.clip_point(TP::to_point(&offset, &snapshot), Bias::Left);
                        };

                        (start_point..end_point, empty_str.clone())
                    })
                    .sorted_by_key(|(range, _)| range.start)
                    .collect::<Vec<_>>();
                buffer.update(cx, |this, cx| {
                    this.edit(edits, None, cx);
                })
            }
            this.refresh_inline_completion(true, false, window, cx);
            linked_editing_ranges::refresh_linked_ranges(this, window, cx);
        });
    }

    pub fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        self.transact(window, cx, |this, window, cx| {
            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                let line_mode = s.line_mode;
                s.move_with(|map, selection| {
                    if selection.is_empty() && !line_mode {
                        let cursor = movement::right(map, selection.head());
                        selection.end = cursor;
                        selection.reversed = true;
                        selection.goal = SelectionGoal::None;
                    }
                })
            });
            this.insert("", window, cx);
            this.refresh_inline_completion(true, false, window, cx);
        });
    }

    pub fn tab_prev(&mut self, _: &TabPrev, window: &mut Window, cx: &mut Context<Self>) {
        if self.move_to_prev_snippet_tabstop(window, cx) {
            return;
        }

        self.outdent(&Outdent, window, cx);
    }

    pub fn tab(&mut self, _: &Tab, window: &mut Window, cx: &mut Context<Self>) {
        if self.move_to_next_snippet_tabstop(window, cx) || self.read_only(cx) {
            return;
        }

        let mut selections = self.selections.all_adjusted(cx);
        let buffer = self.buffer.read(cx);
        let snapshot = buffer.snapshot(cx);
        let rows_iter = selections.iter().map(|s| s.head().row);
        let suggested_indents = snapshot.suggested_indents(rows_iter, cx);

        let mut edits = Vec::new();
        let mut prev_edited_row = 0;
        let mut row_delta = 0;
        for selection in &mut selections {
            if selection.start.row != prev_edited_row {
                row_delta = 0;
            }
            prev_edited_row = selection.end.row;

            // If the selection is non-empty, then increase the indentation of the selected lines.
            if !selection.is_empty() {
                row_delta =
                    Self::indent_selection(buffer, &snapshot, selection, &mut edits, row_delta, cx);
                continue;
            }

            // If the selection is empty and the cursor is in the leading whitespace before the
            // suggested indentation, then auto-indent the line.
            let cursor = selection.head();
            let current_indent = snapshot.indent_size_for_line(MultiBufferRow(cursor.row));
            if let Some(suggested_indent) =
                suggested_indents.get(&MultiBufferRow(cursor.row)).copied()
            {
                if cursor.column < suggested_indent.len
                    && cursor.column <= current_indent.len
                    && current_indent.len <= suggested_indent.len
                {
                    selection.start = Point::new(cursor.row, suggested_indent.len);
                    selection.end = selection.start;
                    if row_delta == 0 {
                        edits.extend(Buffer::edit_for_indent_size_adjustment(
                            cursor.row,
                            current_indent,
                            suggested_indent,
                        ));
                        row_delta = suggested_indent.len - current_indent.len;
                    }
                    continue;
                }
            }

            // Otherwise, insert a hard or soft tab.
            let settings = buffer.settings_at(cursor, cx);
            let tab_size = if settings.hard_tabs {
                IndentSize::tab()
            } else {
                let tab_size = settings.tab_size.get();
                let char_column = snapshot
                    .text_for_range(Point::new(cursor.row, 0)..cursor)
                    .flat_map(str::chars)
                    .count()
                    + row_delta as usize;
                let chars_to_next_tab_stop = tab_size - (char_column as u32 % tab_size);
                IndentSize::spaces(chars_to_next_tab_stop)
            };
            selection.start = Point::new(cursor.row, cursor.column + row_delta + tab_size.len);
            selection.end = selection.start;
            edits.push((cursor..cursor, tab_size.chars().collect::<String>()));
            row_delta += tab_size.len;
        }

        self.transact(window, cx, |this, window, cx| {
            this.buffer.update(cx, |b, cx| b.edit(edits, None, cx));
            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select(selections)
            });
            this.refresh_inline_completion(true, false, window, cx);
        });
    }

    pub fn indent(&mut self, _: &Indent, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only(cx) {
            return;
        }
        let mut selections = self.selections.all::<Point>(cx);
        let mut prev_edited_row = 0;
        let mut row_delta = 0;
        let mut edits = Vec::new();
        let buffer = self.buffer.read(cx);
        let snapshot = buffer.snapshot(cx);
        for selection in &mut selections {
            if selection.start.row != prev_edited_row {
                row_delta = 0;
            }
            prev_edited_row = selection.end.row;

            row_delta =
                Self::indent_selection(buffer, &snapshot, selection, &mut edits, row_delta, cx);
        }

        self.transact(window, cx, |this, window, cx| {
            this.buffer.update(cx, |b, cx| b.edit(edits, None, cx));
            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select(selections)
            });
        });
    }

    fn indent_selection(
        buffer: &MultiBuffer,
        snapshot: &MultiBufferSnapshot,
        selection: &mut Selection<Point>,
        edits: &mut Vec<(Range<Point>, String)>,
        delta_for_start_row: u32,
        cx: &App,
    ) -> u32 {
        let settings = buffer.settings_at(selection.start, cx);
        let tab_size = settings.tab_size.get();
        let indent_kind = if settings.hard_tabs {
            IndentKind::Tab
        } else {
            IndentKind::Space
        };
        let mut start_row = selection.start.row;
        let mut end_row = selection.end.row + 1;

        // If a selection ends at the beginning of a line, don't indent
        // that last line.
        if selection.end.column == 0 && selection.end.row > selection.start.row {
            end_row -= 1;
        }

        // Avoid re-indenting a row that has already been indented by a
        // previous selection, but still update this selection's column
        // to reflect that indentation.
        if delta_for_start_row > 0 {
            start_row += 1;
            selection.start.column += delta_for_start_row;
            if selection.end.row == selection.start.row {
                selection.end.column += delta_for_start_row;
            }
        }

        let mut delta_for_end_row = 0;
        let has_multiple_rows = start_row + 1 != end_row;
        for row in start_row..end_row {
            let current_indent = snapshot.indent_size_for_line(MultiBufferRow(row));
            let indent_delta = match (current_indent.kind, indent_kind) {
                (IndentKind::Space, IndentKind::Space) => {
                    let columns_to_next_tab_stop = tab_size - (current_indent.len % tab_size);
                    IndentSize::spaces(columns_to_next_tab_stop)
                }
                (IndentKind::Tab, IndentKind::Space) => IndentSize::spaces(tab_size),
                (_, IndentKind::Tab) => IndentSize::tab(),
            };

            let start = if has_multiple_rows || current_indent.len < selection.start.column {
                0
            } else {
                selection.start.column
            };
            let row_start = Point::new(row, start);
            edits.push((
                row_start..row_start,
                indent_delta.chars().collect::<String>(),
            ));

            // Update this selection's endpoints to reflect the indentation.
            if row == selection.start.row {
                selection.start.column += indent_delta.len;
            }
            if row == selection.end.row {
                selection.end.column += indent_delta.len;
                delta_for_end_row = indent_delta.len;
            }
        }

        if selection.start.row == selection.end.row {
            delta_for_start_row + delta_for_end_row
        } else {
            delta_for_end_row
        }
    }

    pub fn outdent(&mut self, _: &Outdent, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only(cx) {
            return;
        }
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let selections = self.selections.all::<Point>(cx);
        let mut deletion_ranges = Vec::new();
        let mut last_outdent = None;
        {
            let buffer = self.buffer.read(cx);
            let snapshot = buffer.snapshot(cx);
            for selection in &selections {
                let settings = buffer.settings_at(selection.start, cx);
                let tab_size = settings.tab_size.get();
                let mut rows = selection.spanned_rows(false, &display_map);

                // Avoid re-outdenting a row that has already been outdented by a
                // previous selection.
                if let Some(last_row) = last_outdent {
                    if last_row == rows.start {
                        rows.start = rows.start.next_row();
                    }
                }
                let has_multiple_rows = rows.len() > 1;
                for row in rows.iter_rows() {
                    let indent_size = snapshot.indent_size_for_line(row);
                    if indent_size.len > 0 {
                        let deletion_len = match indent_size.kind {
                            IndentKind::Space => {
                                let columns_to_prev_tab_stop = indent_size.len % tab_size;
                                if columns_to_prev_tab_stop == 0 {
                                    tab_size
                                } else {
                                    columns_to_prev_tab_stop
                                }
                            }
                            IndentKind::Tab => 1,
                        };
                        let start = if has_multiple_rows
                            || deletion_len > selection.start.column
                            || indent_size.len < selection.start.column
                        {
                            0
                        } else {
                            selection.start.column - deletion_len
                        };
                        deletion_ranges.push(
                            Point::new(row.0, start)..Point::new(row.0, start + deletion_len),
                        );
                        last_outdent = Some(row);
                    }
                }
            }
        }

        self.transact(window, cx, |this, window, cx| {
            this.buffer.update(cx, |buffer, cx| {
                let empty_str: Arc<str> = Arc::default();
                buffer.edit(
                    deletion_ranges
                        .into_iter()
                        .map(|range| (range, empty_str.clone())),
                    None,
                    cx,
                );
            });
            let selections = this.selections.all::<usize>(cx);
            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select(selections)
            });
        });
    }

    pub fn autoindent(&mut self, _: &AutoIndent, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only(cx) {
            return;
        }
        let selections = self
            .selections
            .all::<usize>(cx)
            .into_iter()
            .map(|s| s.range());

        self.transact(window, cx, |this, window, cx| {
            this.buffer.update(cx, |buffer, cx| {
                buffer.autoindent_ranges(selections, cx);
            });
            let selections = this.selections.all::<usize>(cx);
            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select(selections)
            });
        });
    }

    pub fn delete_line(&mut self, _: &DeleteLine, window: &mut Window, cx: &mut Context<Self>) {
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let selections = self.selections.all::<Point>(cx);

        let mut new_cursors = Vec::new();
        let mut edit_ranges = Vec::new();
        let mut selections = selections.iter().peekable();
        while let Some(selection) = selections.next() {
            let mut rows = selection.spanned_rows(false, &display_map);
            let goal_display_column = selection.head().to_display_point(&display_map).column();

            // Accumulate contiguous regions of rows that we want to delete.
            while let Some(next_selection) = selections.peek() {
                let next_rows = next_selection.spanned_rows(false, &display_map);
                if next_rows.start <= rows.end {
                    rows.end = next_rows.end;
                    selections.next().unwrap();
                } else {
                    break;
                }
            }

            let buffer = &display_map.buffer_snapshot;
            let mut edit_start = Point::new(rows.start.0, 0).to_offset(buffer);
            let edit_end;
            let cursor_buffer_row;
            if buffer.max_point().row >= rows.end.0 {
                // If there's a line after the range, delete the \n from the end of the row range
                // and position the cursor on the next line.
                edit_end = Point::new(rows.end.0, 0).to_offset(buffer);
                cursor_buffer_row = rows.end;
            } else {
                // If there isn't a line after the range, delete the \n from the line before the
                // start of the row range and position the cursor there.
                edit_start = edit_start.saturating_sub(1);
                edit_end = buffer.len();
                cursor_buffer_row = rows.start.previous_row();
            }

            let mut cursor = Point::new(cursor_buffer_row.0, 0).to_display_point(&display_map);
            *cursor.column_mut() =
                cmp::min(goal_display_column, display_map.line_len(cursor.row()));

            new_cursors.push((
                selection.id,
                buffer.anchor_after(cursor.to_point(&display_map)),
            ));
            edit_ranges.push(edit_start..edit_end);
        }

        self.transact(window, cx, |this, window, cx| {
            let buffer = this.buffer.update(cx, |buffer, cx| {
                let empty_str: Arc<str> = Arc::default();
                buffer.edit(
                    edit_ranges
                        .into_iter()
                        .map(|range| (range, empty_str.clone())),
                    None,
                    cx,
                );
                buffer.snapshot(cx)
            });
            let new_selections = new_cursors
                .into_iter()
                .map(|(id, cursor)| {
                    let cursor = cursor.to_point(&buffer);
                    Selection {
                        id,
                        start: cursor,
                        end: cursor,
                        reversed: false,
                        goal: SelectionGoal::None,
                    }
                })
                .collect();

            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select(new_selections);
            });
        });
    }

    pub fn join_lines_impl(
        &mut self,
        insert_whitespace: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }
        let mut row_ranges = Vec::<Range<MultiBufferRow>>::new();
        for selection in self.selections.all::<Point>(cx) {
            let start = MultiBufferRow(selection.start.row);
            // Treat single line selections as if they include the next line. Otherwise this action
            // would do nothing for single line selections individual cursors.
            let end = if selection.start.row == selection.end.row {
                MultiBufferRow(selection.start.row + 1)
            } else {
                MultiBufferRow(selection.end.row)
            };

            if let Some(last_row_range) = row_ranges.last_mut() {
                if start <= last_row_range.end {
                    last_row_range.end = end;
                    continue;
                }
            }
            row_ranges.push(start..end);
        }

        let snapshot = self.buffer.read(cx).snapshot(cx);
        let mut cursor_positions = Vec::new();
        for row_range in &row_ranges {
            let anchor = snapshot.anchor_before(Point::new(
                row_range.end.previous_row().0,
                snapshot.line_len(row_range.end.previous_row()),
            ));
            cursor_positions.push(anchor..anchor);
        }

        self.transact(window, cx, |this, window, cx| {
            for row_range in row_ranges.into_iter().rev() {
                for row in row_range.iter_rows().rev() {
                    let end_of_line = Point::new(row.0, snapshot.line_len(row));
                    let next_line_row = row.next_row();
                    let indent = snapshot.indent_size_for_line(next_line_row);
                    let start_of_next_line = Point::new(next_line_row.0, indent.len);

                    let replace =
                        if snapshot.line_len(next_line_row) > indent.len && insert_whitespace {
                            " "
                        } else {
                            ""
                        };

                    this.buffer.update(cx, |buffer, cx| {
                        buffer.edit([(end_of_line..start_of_next_line, replace)], None, cx)
                    });
                }
            }

            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select_anchor_ranges(cursor_positions)
            });
        });
    }

    pub fn join_lines(&mut self, _: &JoinLines, window: &mut Window, cx: &mut Context<Self>) {
        self.join_lines_impl(true, window, cx);
    }

    pub fn sort_lines_case_sensitive(
        &mut self,
        _: &SortLinesCaseSensitive,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_lines(window, cx, |lines| lines.sort())
    }

    pub fn sort_lines_case_insensitive(
        &mut self,
        _: &SortLinesCaseInsensitive,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_lines(window, cx, |lines| {
            lines.sort_by_key(|line| line.to_lowercase())
        })
    }

    pub fn unique_lines_case_insensitive(
        &mut self,
        _: &UniqueLinesCaseInsensitive,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_lines(window, cx, |lines| {
            let mut seen = HashSet::default();
            lines.retain(|line| seen.insert(line.to_lowercase()));
        })
    }

    pub fn unique_lines_case_sensitive(
        &mut self,
        _: &UniqueLinesCaseSensitive,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_lines(window, cx, |lines| {
            let mut seen = HashSet::default();
            lines.retain(|line| seen.insert(*line));
        })
    }

    pub fn revert_file(&mut self, _: &RevertFile, window: &mut Window, cx: &mut Context<Self>) {
        let mut revert_changes = HashMap::default();
        let snapshot = self.snapshot(window, cx);
        for hunk in snapshot
            .hunks_for_ranges(Some(Point::zero()..snapshot.buffer_snapshot.max_point()).into_iter())
        {
            self.prepare_revert_change(&mut revert_changes, &hunk, cx);
        }
        if !revert_changes.is_empty() {
            self.transact(window, cx, |editor, window, cx| {
                editor.revert(revert_changes, window, cx);
            });
        }
    }

    pub fn reload_file(&mut self, _: &ReloadFile, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            return;
        };
        self.reload(project, window, cx)
            .detach_and_notify_err(window, cx);
    }

    pub fn revert_selected_hunks(
        &mut self,
        _: &RevertSelectedHunks,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selections = self.selections.all(cx).into_iter().map(|s| s.range());
        self.revert_hunks_in_ranges(selections, window, cx);
    }

    fn revert_hunks_in_ranges(
        &mut self,
        ranges: impl Iterator<Item = Range<Point>>,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) {
        let mut revert_changes = HashMap::default();
        let snapshot = self.snapshot(window, cx);
        for hunk in &snapshot.hunks_for_ranges(ranges) {
            self.prepare_revert_change(&mut revert_changes, &hunk, cx);
        }
        if !revert_changes.is_empty() {
            self.transact(window, cx, |editor, window, cx| {
                editor.revert(revert_changes, window, cx);
            });
        }
    }

    pub fn open_active_item_in_terminal(
        &mut self,
        _: &OpenInTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(working_directory) = self.active_excerpt(cx).and_then(|(_, buffer, _)| {
            let project_path = buffer.read(cx).project_path(cx)?;
            let project = self.project.as_ref()?.read(cx);
            let entry = project.entry_for_path(&project_path, cx)?;
            let parent = match &entry.canonical_path {
                Some(canonical_path) => canonical_path.to_path_buf(),
                None => project.absolute_path(&project_path, cx)?,
            }
            .parent()?
            .to_path_buf();
            Some(parent)
        }) {
            window.dispatch_action(OpenTerminal { working_directory }.boxed_clone(), cx);
        }
    }

    pub fn prepare_revert_change(
        &self,
        revert_changes: &mut HashMap<BufferId, Vec<(Range<text::Anchor>, Rope)>>,
        hunk: &MultiBufferDiffHunk,
        cx: &mut App,
    ) -> Option<()> {
        let buffer = self.buffer.read(cx);
        let diff = buffer.diff_for(hunk.buffer_id)?;
        let buffer = buffer.buffer(hunk.buffer_id)?;
        let buffer = buffer.read(cx);
        let original_text = diff
            .read(cx)
            .snapshot
            .base_text
            .as_ref()?
            .as_rope()
            .slice(hunk.diff_base_byte_range.clone());
        let buffer_snapshot = buffer.snapshot();
        let buffer_revert_changes = revert_changes.entry(buffer.remote_id()).or_default();
        if let Err(i) = buffer_revert_changes.binary_search_by(|probe| {
            probe
                .0
                .start
                .cmp(&hunk.buffer_range.start, &buffer_snapshot)
                .then(probe.0.end.cmp(&hunk.buffer_range.end, &buffer_snapshot))
        }) {
            buffer_revert_changes.insert(i, (hunk.buffer_range.clone(), original_text));
            Some(())
        } else {
            None
        }
    }

    pub fn reverse_lines(&mut self, _: &ReverseLines, window: &mut Window, cx: &mut Context<Self>) {
        self.manipulate_lines(window, cx, |lines| lines.reverse())
    }

    pub fn shuffle_lines(&mut self, _: &ShuffleLines, window: &mut Window, cx: &mut Context<Self>) {
        self.manipulate_lines(window, cx, |lines| lines.shuffle(&mut thread_rng()))
    }

    fn manipulate_lines<Fn>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        mut callback: Fn,
    ) where
        Fn: FnMut(&mut Vec<&str>),
    {
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let buffer = self.buffer.read(cx).snapshot(cx);

        let mut edits = Vec::new();

        let selections = self.selections.all::<Point>(cx);
        let mut selections = selections.iter().peekable();
        let mut contiguous_row_selections = Vec::new();
        let mut new_selections = Vec::new();
        let mut added_lines = 0;
        let mut removed_lines = 0;

        while let Some(selection) = selections.next() {
            let (start_row, end_row) = consume_contiguous_rows(
                &mut contiguous_row_selections,
                selection,
                &display_map,
                &mut selections,
            );

            let start_point = Point::new(start_row.0, 0);
            let end_point = Point::new(
                end_row.previous_row().0,
                buffer.line_len(end_row.previous_row()),
            );
            let text = buffer
                .text_for_range(start_point..end_point)
                .collect::<String>();

            let mut lines = text.split('\n').collect_vec();

            let lines_before = lines.len();
            callback(&mut lines);
            let lines_after = lines.len();

            edits.push((start_point..end_point, lines.join("\n")));

            // Selections must change based on added and removed line count
            let start_row =
                MultiBufferRow(start_point.row + added_lines as u32 - removed_lines as u32);
            let end_row = MultiBufferRow(start_row.0 + lines_after.saturating_sub(1) as u32);
            new_selections.push(Selection {
                id: selection.id,
                start: start_row,
                end: end_row,
                goal: SelectionGoal::None,
                reversed: selection.reversed,
            });

            if lines_after > lines_before {
                added_lines += lines_after - lines_before;
            } else if lines_before > lines_after {
                removed_lines += lines_before - lines_after;
            }
        }

        self.transact(window, cx, |this, window, cx| {
            let buffer = this.buffer.update(cx, |buffer, cx| {
                buffer.edit(edits, None, cx);
                buffer.snapshot(cx)
            });

            // Recalculate offsets on newly edited buffer
            let new_selections = new_selections
                .iter()
                .map(|s| {
                    let start_point = Point::new(s.start.0, 0);
                    let end_point = Point::new(s.end.0, buffer.line_len(s.end));
                    Selection {
                        id: s.id,
                        start: buffer.point_to_offset(start_point),
                        end: buffer.point_to_offset(end_point),
                        goal: s.goal,
                        reversed: s.reversed,
                    }
                })
                .collect();

            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select(new_selections);
            });

            this.request_autoscroll(Autoscroll::fit(), cx);
        });
    }

    pub fn convert_to_upper_case(
        &mut self,
        _: &ConvertToUpperCase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_text(window, cx, |text| text.to_uppercase())
    }

    pub fn convert_to_lower_case(
        &mut self,
        _: &ConvertToLowerCase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_text(window, cx, |text| text.to_lowercase())
    }

    pub fn convert_to_title_case(
        &mut self,
        _: &ConvertToTitleCase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_text(window, cx, |text| {
            text.split('\n')
                .map(|line| line.to_case(Case::Title))
                .join("\n")
        })
    }

    pub fn convert_to_snake_case(
        &mut self,
        _: &ConvertToSnakeCase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_text(window, cx, |text| text.to_case(Case::Snake))
    }

    pub fn convert_to_kebab_case(
        &mut self,
        _: &ConvertToKebabCase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_text(window, cx, |text| text.to_case(Case::Kebab))
    }

    pub fn convert_to_upper_camel_case(
        &mut self,
        _: &ConvertToUpperCamelCase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_text(window, cx, |text| {
            text.split('\n')
                .map(|line| line.to_case(Case::UpperCamel))
                .join("\n")
        })
    }

    pub fn convert_to_lower_camel_case(
        &mut self,
        _: &ConvertToLowerCamelCase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_text(window, cx, |text| text.to_case(Case::Camel))
    }

    pub fn convert_to_opposite_case(
        &mut self,
        _: &ConvertToOppositeCase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_text(window, cx, |text| {
            text.chars()
                .fold(String::with_capacity(text.len()), |mut t, c| {
                    if c.is_uppercase() {
                        t.extend(c.to_lowercase());
                    } else {
                        t.extend(c.to_uppercase());
                    }
                    t
                })
        })
    }

    fn manipulate_text<Fn>(&mut self, window: &mut Window, cx: &mut Context<Self>, mut callback: Fn)
    where
        Fn: FnMut(&str) -> String,
    {
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let buffer = self.buffer.read(cx).snapshot(cx);

        let mut new_selections = Vec::new();
        let mut edits = Vec::new();
        let mut selection_adjustment = 0i32;

        for selection in self.selections.all::<usize>(cx) {
            let selection_is_empty = selection.is_empty();

            let (start, end) = if selection_is_empty {
                let word_range = movement::surrounding_word(
                    &display_map,
                    selection.start.to_display_point(&display_map),
                );
                let start = word_range.start.to_offset(&display_map, Bias::Left);
                let end = word_range.end.to_offset(&display_map, Bias::Left);
                (start, end)
            } else {
                (selection.start, selection.end)
            };

            let text = buffer.text_for_range(start..end).collect::<String>();
            let old_length = text.len() as i32;
            let text = callback(&text);

            new_selections.push(Selection {
                start: (start as i32 - selection_adjustment) as usize,
                end: ((start + text.len()) as i32 - selection_adjustment) as usize,
                goal: SelectionGoal::None,
                ..selection
            });

            selection_adjustment += old_length - text.len() as i32;

            edits.push((start..end, text));
        }

        self.transact(window, cx, |this, window, cx| {
            this.buffer.update(cx, |buffer, cx| {
                buffer.edit(edits, None, cx);
            });

            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select(new_selections);
            });

            this.request_autoscroll(Autoscroll::fit(), cx);
        });
    }

    pub fn duplicate(
        &mut self,
        upwards: bool,
        whole_lines: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let buffer = &display_map.buffer_snapshot;
        let selections = self.selections.all::<Point>(cx);

        let mut edits = Vec::new();
        let mut selections_iter = selections.iter().peekable();
        while let Some(selection) = selections_iter.next() {
            let mut rows = selection.spanned_rows(false, &display_map);
            // duplicate line-wise
            if whole_lines || selection.start == selection.end {
                // Avoid duplicating the same lines twice.
                while let Some(next_selection) = selections_iter.peek() {
                    let next_rows = next_selection.spanned_rows(false, &display_map);
                    if next_rows.start < rows.end {
                        rows.end = next_rows.end;
                        selections_iter.next().unwrap();
                    } else {
                        break;
                    }
                }

                // Copy the text from the selected row region and splice it either at the start
                // or end of the region.
                let start = Point::new(rows.start.0, 0);
                let end = Point::new(
                    rows.end.previous_row().0,
                    buffer.line_len(rows.end.previous_row()),
                );
                let text = buffer
                    .text_for_range(start..end)
                    .chain(Some("\n"))
                    .collect::<String>();
                let insert_location = if upwards {
                    Point::new(rows.end.0, 0)
                } else {
                    start
                };
                edits.push((insert_location..insert_location, text));
            } else {
                // duplicate character-wise
                let start = selection.start;
                let end = selection.end;
                let text = buffer.text_for_range(start..end).collect::<String>();
                edits.push((selection.end..selection.end, text));
            }
        }

        self.transact(window, cx, |this, _, cx| {
            this.buffer.update(cx, |buffer, cx| {
                buffer.edit(edits, None, cx);
            });

            this.request_autoscroll(Autoscroll::fit(), cx);
        });
    }

    pub fn duplicate_line_up(
        &mut self,
        _: &DuplicateLineUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.duplicate(true, true, window, cx);
    }

    pub fn duplicate_line_down(
        &mut self,
        _: &DuplicateLineDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.duplicate(false, true, window, cx);
    }

    pub fn duplicate_selection(
        &mut self,
        _: &DuplicateSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.duplicate(false, false, window, cx);
    }

    pub fn move_line_up(&mut self, _: &MoveLineUp, window: &mut Window, cx: &mut Context<Self>) {
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let buffer = self.buffer.read(cx).snapshot(cx);

        let mut edits = Vec::new();
        let mut unfold_ranges = Vec::new();
        let mut refold_creases = Vec::new();

        let selections = self.selections.all::<Point>(cx);
        let mut selections = selections.iter().peekable();
        let mut contiguous_row_selections = Vec::new();
        let mut new_selections = Vec::new();

        while let Some(selection) = selections.next() {
            // Find all the selections that span a contiguous row range
            let (start_row, end_row) = consume_contiguous_rows(
                &mut contiguous_row_selections,
                selection,
                &display_map,
                &mut selections,
            );

            // Move the text spanned by the row range to be before the line preceding the row range
            if start_row.0 > 0 {
                let range_to_move = Point::new(
                    start_row.previous_row().0,
                    buffer.line_len(start_row.previous_row()),
                )
                    ..Point::new(
                        end_row.previous_row().0,
                        buffer.line_len(end_row.previous_row()),
                    );
                let insertion_point = display_map
                    .prev_line_boundary(Point::new(start_row.previous_row().0, 0))
                    .0;

                // Don't move lines across excerpts
                if buffer
                    .excerpt_containing(insertion_point..range_to_move.end)
                    .is_some()
                {
                    let text = buffer
                        .text_for_range(range_to_move.clone())
                        .flat_map(|s| s.chars())
                        .skip(1)
                        .chain(['\n'])
                        .collect::<String>();

                    edits.push((
                        buffer.anchor_after(range_to_move.start)
                            ..buffer.anchor_before(range_to_move.end),
                        String::new(),
                    ));
                    let insertion_anchor = buffer.anchor_after(insertion_point);
                    edits.push((insertion_anchor..insertion_anchor, text));

                    let row_delta = range_to_move.start.row - insertion_point.row + 1;

                    // Move selections up
                    new_selections.extend(contiguous_row_selections.drain(..).map(
                        |mut selection| {
                            selection.start.row -= row_delta;
                            selection.end.row -= row_delta;
                            selection
                        },
                    ));

                    // Move folds up
                    unfold_ranges.push(range_to_move.clone());
                    for fold in display_map.folds_in_range(
                        buffer.anchor_before(range_to_move.start)
                            ..buffer.anchor_after(range_to_move.end),
                    ) {
                        let mut start = fold.range.start.to_point(&buffer);
                        let mut end = fold.range.end.to_point(&buffer);
                        start.row -= row_delta;
                        end.row -= row_delta;
                        refold_creases.push(Crease::simple(start..end, fold.placeholder.clone()));
                    }
                }
            }

            // If we didn't move line(s), preserve the existing selections
            new_selections.append(&mut contiguous_row_selections);
        }

        self.transact(window, cx, |this, window, cx| {
            this.unfold_ranges(&unfold_ranges, true, true, cx);
            this.buffer.update(cx, |buffer, cx| {
                for (range, text) in edits {
                    buffer.edit([(range, text)], None, cx);
                }
            });
            this.fold_creases(refold_creases, true, window, cx);
            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select(new_selections);
            })
        });
    }

    pub fn move_line_down(
        &mut self,
        _: &MoveLineDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let buffer = self.buffer.read(cx).snapshot(cx);

        let mut edits = Vec::new();
        let mut unfold_ranges = Vec::new();
        let mut refold_creases = Vec::new();

        let selections = self.selections.all::<Point>(cx);
        let mut selections = selections.iter().peekable();
        let mut contiguous_row_selections = Vec::new();
        let mut new_selections = Vec::new();

        while let Some(selection) = selections.next() {
            // Find all the selections that span a contiguous row range
            let (start_row, end_row) = consume_contiguous_rows(
                &mut contiguous_row_selections,
                selection,
                &display_map,
                &mut selections,
            );

            // Move the text spanned by the row range to be after the last line of the row range
            if end_row.0 <= buffer.max_point().row {
                let range_to_move =
                    MultiBufferPoint::new(start_row.0, 0)..MultiBufferPoint::new(end_row.0, 0);
                let insertion_point = display_map
                    .next_line_boundary(MultiBufferPoint::new(end_row.0, 0))
                    .0;

                // Don't move lines across excerpt boundaries
                if buffer
                    .excerpt_containing(range_to_move.start..insertion_point)
                    .is_some()
                {
                    let mut text = String::from("\n");
                    text.extend(buffer.text_for_range(range_to_move.clone()));
                    text.pop(); // Drop trailing newline
                    edits.push((
                        buffer.anchor_after(range_to_move.start)
                            ..buffer.anchor_before(range_to_move.end),
                        String::new(),
                    ));
                    let insertion_anchor = buffer.anchor_after(insertion_point);
                    edits.push((insertion_anchor..insertion_anchor, text));

                    let row_delta = insertion_point.row - range_to_move.end.row + 1;

                    // Move selections down
                    new_selections.extend(contiguous_row_selections.drain(..).map(
                        |mut selection| {
                            selection.start.row += row_delta;
                            selection.end.row += row_delta;
                            selection
                        },
                    ));

                    // Move folds down
                    unfold_ranges.push(range_to_move.clone());
                    for fold in display_map.folds_in_range(
                        buffer.anchor_before(range_to_move.start)
                            ..buffer.anchor_after(range_to_move.end),
                    ) {
                        let mut start = fold.range.start.to_point(&buffer);
                        let mut end = fold.range.end.to_point(&buffer);
                        start.row += row_delta;
                        end.row += row_delta;
                        refold_creases.push(Crease::simple(start..end, fold.placeholder.clone()));
                    }
                }
            }

            // If we didn't move line(s), preserve the existing selections
            new_selections.append(&mut contiguous_row_selections);
        }

        self.transact(window, cx, |this, window, cx| {
            this.unfold_ranges(&unfold_ranges, true, true, cx);
            this.buffer.update(cx, |buffer, cx| {
                for (range, text) in edits {
                    buffer.edit([(range, text)], None, cx);
                }
            });
            this.fold_creases(refold_creases, true, window, cx);
            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select(new_selections)
            });
        });
    }

    pub fn transpose(&mut self, _: &Transpose, window: &mut Window, cx: &mut Context<Self>) {
        let text_layout_details = &self.text_layout_details(window);
        self.transact(window, cx, |this, window, cx| {
            let edits = this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                let mut edits: Vec<(Range<usize>, String)> = Default::default();
                let line_mode = s.line_mode;
                s.move_with(|display_map, selection| {
                    if !selection.is_empty() || line_mode {
                        return;
                    }

                    let mut head = selection.head();
                    let mut transpose_offset = head.to_offset(display_map, Bias::Right);
                    if head.column() == display_map.line_len(head.row()) {
                        transpose_offset = display_map
                            .buffer_snapshot
                            .clip_offset(transpose_offset.saturating_sub(1), Bias::Left);
                    }

                    if transpose_offset == 0 {
                        return;
                    }

                    *head.column_mut() += 1;
                    head = display_map.clip_point(head, Bias::Right);
                    let goal = SelectionGoal::HorizontalPosition(
                        display_map
                            .x_for_display_point(head, text_layout_details)
                            .into(),
                    );
                    selection.collapse_to(head, goal);

                    let transpose_start = display_map
                        .buffer_snapshot
                        .clip_offset(transpose_offset.saturating_sub(1), Bias::Left);
                    if edits.last().map_or(true, |e| e.0.end <= transpose_start) {
                        let transpose_end = display_map
                            .buffer_snapshot
                            .clip_offset(transpose_offset + 1, Bias::Right);
                        if let Some(ch) =
                            display_map.buffer_snapshot.chars_at(transpose_start).next()
                        {
                            edits.push((transpose_start..transpose_offset, String::new()));
                            edits.push((transpose_end..transpose_end, ch.to_string()));
                        }
                    }
                });
                edits
            });
            this.buffer
                .update(cx, |buffer, cx| buffer.edit(edits, None, cx));
            let selections = this.selections.all::<usize>(cx);
            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select(selections);
            });
        });
    }

    pub fn rewrap(&mut self, _: &Rewrap, _: &mut Window, cx: &mut Context<Self>) {
        self.rewrap_impl(IsVimMode::No, cx)
    }

    pub fn rewrap_impl(&mut self, is_vim_mode: IsVimMode, cx: &mut Context<Self>) {
        let buffer = self.buffer.read(cx).snapshot(cx);
        let selections = self.selections.all::<Point>(cx);
        let mut selections = selections.iter().peekable();

        let mut edits = Vec::new();
        let mut rewrapped_row_ranges = Vec::<RangeInclusive<u32>>::new();

        while let Some(selection) = selections.next() {
            let mut start_row = selection.start.row;
            let mut end_row = selection.end.row;

            // Skip selections that overlap with a range that has already been rewrapped.
            let selection_range = start_row..end_row;
            if rewrapped_row_ranges
                .iter()
                .any(|range| range.overlaps(&selection_range))
            {
                continue;
            }

            let mut should_rewrap = is_vim_mode == IsVimMode::Yes;

            if let Some(language_scope) = buffer.language_scope_at(selection.head()) {
                match language_scope.language_name().as_ref() {
                    "Markdown" | "Plain Text" => {
                        should_rewrap = true;
                    }
                    _ => {}
                }
            }

            let tab_size = buffer.settings_at(selection.head(), cx).tab_size;

            // Since not all lines in the selection may be at the same indent
            // level, choose the indent size that is the most common between all
            // of the lines.
            //
            // If there is a tie, we use the deepest indent.
            let (indent_size, indent_end) = {
                let mut indent_size_occurrences = HashMap::default();
                let mut rows_by_indent_size = HashMap::<IndentSize, Vec<u32>>::default();

                for row in start_row..=end_row {
                    let indent = buffer.indent_size_for_line(MultiBufferRow(row));
                    rows_by_indent_size.entry(indent).or_default().push(row);
                    *indent_size_occurrences.entry(indent).or_insert(0) += 1;
                }

                let indent_size = indent_size_occurrences
                    .into_iter()
                    .max_by_key(|(indent, count)| (*count, indent.len_with_expanded_tabs(tab_size)))
                    .map(|(indent, _)| indent)
                    .unwrap_or_default();
                let row = rows_by_indent_size[&indent_size][0];
                let indent_end = Point::new(row, indent_size.len);

                (indent_size, indent_end)
            };

            let mut line_prefix = indent_size.chars().collect::<String>();

            if let Some(comment_prefix) =
                buffer
                    .language_scope_at(selection.head())
                    .and_then(|language| {
                        language
                            .line_comment_prefixes()
                            .iter()
                            .find(|prefix| buffer.contains_str_at(indent_end, prefix))
                            .cloned()
                    })
            {
                line_prefix.push_str(&comment_prefix);
                should_rewrap = true;
            }

            if !should_rewrap {
                continue;
            }

            if selection.is_empty() {
                'expand_upwards: while start_row > 0 {
                    let prev_row = start_row - 1;
                    if buffer.contains_str_at(Point::new(prev_row, 0), &line_prefix)
                        && buffer.line_len(MultiBufferRow(prev_row)) as usize > line_prefix.len()
                    {
                        start_row = prev_row;
                    } else {
                        break 'expand_upwards;
                    }
                }

                'expand_downwards: while end_row < buffer.max_point().row {
                    let next_row = end_row + 1;
                    if buffer.contains_str_at(Point::new(next_row, 0), &line_prefix)
                        && buffer.line_len(MultiBufferRow(next_row)) as usize > line_prefix.len()
                    {
                        end_row = next_row;
                    } else {
                        break 'expand_downwards;
                    }
                }
            }

            let start = Point::new(start_row, 0);
            let end = Point::new(end_row, buffer.line_len(MultiBufferRow(end_row)));
            let selection_text = buffer.text_for_range(start..end).collect::<String>();
            let Some(lines_without_prefixes) = selection_text
                .lines()
                .map(|line| {
                    line.strip_prefix(&line_prefix)
                        .or_else(|| line.trim_start().strip_prefix(&line_prefix.trim_start()))
                        .ok_or_else(|| {
                            anyhow!("line did not start with prefix {line_prefix:?}: {line:?}")
                        })
                })
                .collect::<Result<Vec<_>, _>>()
                .log_err()
            else {
                continue;
            };

            let wrap_column = buffer
                .settings_at(Point::new(start_row, 0), cx)
                .preferred_line_length as usize;
            let wrapped_text = wrap_with_prefix(
                line_prefix,
                lines_without_prefixes.join(" "),
                wrap_column,
                tab_size,
            );

            // TODO: should always use char-based diff while still supporting cursor behavior that
            // matches vim.
            let diff = match is_vim_mode {
                IsVimMode::Yes => TextDiff::from_lines(&selection_text, &wrapped_text),
                IsVimMode::No => TextDiff::from_chars(&selection_text, &wrapped_text),
            };
            let mut offset = start.to_offset(&buffer);
            let mut moved_since_edit = true;

            for change in diff.iter_all_changes() {
                let value = change.value();
                match change.tag() {
                    ChangeTag::Equal => {
                        offset += value.len();
                        moved_since_edit = true;
                    }
                    ChangeTag::Delete => {
                        let start = buffer.anchor_after(offset);
                        let end = buffer.anchor_before(offset + value.len());

                        if moved_since_edit {
                            edits.push((start..end, String::new()));
                        } else {
                            edits.last_mut().unwrap().0.end = end;
                        }

                        offset += value.len();
                        moved_since_edit = false;
                    }
                    ChangeTag::Insert => {
                        if moved_since_edit {
                            let anchor = buffer.anchor_after(offset);
                            edits.push((anchor..anchor, value.to_string()));
                        } else {
                            edits.last_mut().unwrap().1.push_str(value);
                        }

                        moved_since_edit = false;
                    }
                }
            }

            rewrapped_row_ranges.push(start_row..=end_row);
        }

        self.buffer
            .update(cx, |buffer, cx| buffer.edit(edits, None, cx));
    }

    pub fn cut_common(&mut self, window: &mut Window, cx: &mut Context<Self>) -> ClipboardItem {
        let mut text = String::new();
        let buffer = self.buffer.read(cx).snapshot(cx);
        let mut selections = self.selections.all::<Point>(cx);
        let mut clipboard_selections = Vec::with_capacity(selections.len());
        {
            let max_point = buffer.max_point();
            let mut is_first = true;
            for selection in &mut selections {
                let is_entire_line = selection.is_empty() || self.selections.line_mode;
                if is_entire_line {
                    selection.start = Point::new(selection.start.row, 0);
                    if !selection.is_empty() && selection.end.column == 0 {
                        selection.end = cmp::min(max_point, selection.end);
                    } else {
                        selection.end = cmp::min(max_point, Point::new(selection.end.row + 1, 0));
                    }
                    selection.goal = SelectionGoal::None;
                }
                if is_first {
                    is_first = false;
                } else {
                    text += "\n";
                }
                let mut len = 0;
                for chunk in buffer.text_for_range(selection.start..selection.end) {
                    text.push_str(chunk);
                    len += chunk.len();
                }
                clipboard_selections.push(ClipboardSelection {
                    len,
                    is_entire_line,
                    first_line_indent: buffer
                        .indent_size_for_line(MultiBufferRow(selection.start.row))
                        .len,
                });
            }
        }

        self.transact(window, cx, |this, window, cx| {
            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select(selections);
            });
            this.insert("", window, cx);
        });
        ClipboardItem::new_string_with_json_metadata(text, clipboard_selections)
    }

    pub fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        let item = self.cut_common(window, cx);
        cx.write_to_clipboard(item);
    }

    pub fn kill_ring_cut(&mut self, _: &KillRingCut, window: &mut Window, cx: &mut Context<Self>) {
        self.change_selections(None, window, cx, |s| {
            s.move_with(|snapshot, sel| {
                if sel.is_empty() {
                    sel.end = DisplayPoint::new(sel.end.row(), snapshot.line_len(sel.end.row()))
                }
            });
        });
        let item = self.cut_common(window, cx);
        cx.set_global(KillRing(item))
    }

    pub fn kill_ring_yank(
        &mut self,
        _: &KillRingYank,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (text, metadata) = if let Some(KillRing(item)) = cx.try_global() {
            if let Some(ClipboardEntry::String(kill_ring)) = item.entries().first() {
                (kill_ring.text().to_string(), kill_ring.metadata_json())
            } else {
                return;
            }
        } else {
            return;
        };
        self.do_paste(&text, metadata, false, window, cx);
    }

    pub fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let selections = self.selections.all::<Point>(cx);
        let buffer = self.buffer.read(cx).read(cx);
        let mut text = String::new();

        let mut clipboard_selections = Vec::with_capacity(selections.len());
        {
            let max_point = buffer.max_point();
            let mut is_first = true;
            for selection in selections.iter() {
                let mut start = selection.start;
                let mut end = selection.end;
                let is_entire_line = selection.is_empty() || self.selections.line_mode;
                if is_entire_line {
                    start = Point::new(start.row, 0);
                    end = cmp::min(max_point, Point::new(end.row + 1, 0));
                }
                if is_first {
                    is_first = false;
                } else {
                    text += "\n";
                }
                let mut len = 0;
                for chunk in buffer.text_for_range(start..end) {
                    text.push_str(chunk);
                    len += chunk.len();
                }
                clipboard_selections.push(ClipboardSelection {
                    len,
                    is_entire_line,
                    first_line_indent: buffer.indent_size_for_line(MultiBufferRow(start.row)).len,
                });
            }
        }

        cx.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(
            text,
            clipboard_selections,
        ));
    }

    pub fn do_paste(
        &mut self,
        text: &String,
        clipboard_selections: Option<Vec<ClipboardSelection>>,
        handle_entire_lines: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }

        let clipboard_text = Cow::Borrowed(text);

        self.transact(window, cx, |this, window, cx| {
            if let Some(mut clipboard_selections) = clipboard_selections {
                let old_selections = this.selections.all::<usize>(cx);
                let all_selections_were_entire_line =
                    clipboard_selections.iter().all(|s| s.is_entire_line);
                let first_selection_indent_column =
                    clipboard_selections.first().map(|s| s.first_line_indent);
                if clipboard_selections.len() != old_selections.len() {
                    clipboard_selections.drain(..);
                }
                let cursor_offset = this.selections.last::<usize>(cx).head();
                let mut auto_indent_on_paste = true;

                this.buffer.update(cx, |buffer, cx| {
                    let snapshot = buffer.read(cx);
                    auto_indent_on_paste =
                        snapshot.settings_at(cursor_offset, cx).auto_indent_on_paste;

                    let mut start_offset = 0;
                    let mut edits = Vec::new();
                    let mut original_indent_columns = Vec::new();
                    for (ix, selection) in old_selections.iter().enumerate() {
                        let to_insert;
                        let entire_line;
                        let original_indent_column;
                        if let Some(clipboard_selection) = clipboard_selections.get(ix) {
                            let end_offset = start_offset + clipboard_selection.len;
                            to_insert = &clipboard_text[start_offset..end_offset];
                            entire_line = clipboard_selection.is_entire_line;
                            start_offset = end_offset + 1;
                            original_indent_column = Some(clipboard_selection.first_line_indent);
                        } else {
                            to_insert = clipboard_text.as_str();
                            entire_line = all_selections_were_entire_line;
                            original_indent_column = first_selection_indent_column
                        }

                        // If the corresponding selection was empty when this slice of the
                        // clipboard text was written, then the entire line containing the
                        // selection was copied. If this selection is also currently empty,
                        // then paste the line before the current line of the buffer.
                        let range = if selection.is_empty() && handle_entire_lines && entire_line {
                            let column = selection.start.to_point(&snapshot).column as usize;
                            let line_start = selection.start - column;
                            line_start..line_start
                        } else {
                            selection.range()
                        };

                        edits.push((range, to_insert));
                        original_indent_columns.extend(original_indent_column);
                    }
                    drop(snapshot);

                    buffer.edit(
                        edits,
                        if auto_indent_on_paste {
                            Some(AutoindentMode::Block {
                                original_indent_columns,
                            })
                        } else {
                            None
                        },
                        cx,
                    );
                });

                let selections = this.selections.all::<usize>(cx);
                this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                    s.select(selections)
                });
            } else {
                this.insert(&clipboard_text, window, cx);
            }
        });
    }

    pub fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = cx.read_from_clipboard() {
            let entries = item.entries();

            match entries.first() {
                // For now, we only support applying metadata if there's one string. In the future, we can incorporate all the selections
                // of all the pasted entries.
                Some(ClipboardEntry::String(clipboard_string)) if entries.len() == 1 => self
                    .do_paste(
                        clipboard_string.text(),
                        clipboard_string.metadata_json::<Vec<ClipboardSelection>>(),
                        true,
                        window,
                        cx,
                    ),
                _ => self.do_paste(&item.text().unwrap_or_default(), None, true, window, cx),
            }
        }
    }

    pub fn undo(&mut self, _: &Undo, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only(cx) {
            return;
        }

        if let Some(transaction_id) = self.buffer.update(cx, |buffer, cx| buffer.undo(cx)) {
            if let Some((selections, _)) =
                self.selection_history.transaction(transaction_id).cloned()
            {
                self.change_selections(None, window, cx, |s| {
                    s.select_anchors(selections.to_vec());
                });
            }
            self.request_autoscroll(Autoscroll::fit(), cx);
            self.unmark_text(window, cx);
            self.refresh_inline_completion(true, false, window, cx);
            cx.emit(EditorEvent::Edited { transaction_id });
            cx.emit(EditorEvent::TransactionUndone { transaction_id });
        }
    }

    pub fn redo(&mut self, _: &Redo, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only(cx) {
            return;
        }

        if let Some(transaction_id) = self.buffer.update(cx, |buffer, cx| buffer.redo(cx)) {
            if let Some((_, Some(selections))) =
                self.selection_history.transaction(transaction_id).cloned()
            {
                self.change_selections(None, window, cx, |s| {
                    s.select_anchors(selections.to_vec());
                });
            }
            self.request_autoscroll(Autoscroll::fit(), cx);
            self.unmark_text(window, cx);
            self.refresh_inline_completion(true, false, window, cx);
            cx.emit(EditorEvent::Edited { transaction_id });
        }
    }

    pub fn finalize_last_transaction(&mut self, cx: &mut Context<Self>) {
        self.buffer
            .update(cx, |buffer, cx| buffer.finalize_last_transaction(cx));
    }

    pub fn group_until_transaction(&mut self, tx_id: TransactionId, cx: &mut Context<Self>) {
        self.buffer
            .update(cx, |buffer, cx| buffer.group_until_transaction(tx_id, cx));
    }

    pub fn move_left(&mut self, _: &MoveLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            let line_mode = s.line_mode;
            s.move_with(|map, selection| {
                let cursor = if selection.is_empty() && !line_mode {
                    movement::left(map, selection.start)
                } else {
                    selection.start
                };
                selection.collapse_to(cursor, SelectionGoal::None);
            });
        })
    }

    pub fn select_left(&mut self, _: &SelectLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_heads_with(|map, head, _| (movement::left(map, head), SelectionGoal::None));
        })
    }

    pub fn move_right(&mut self, _: &MoveRight, window: &mut Window, cx: &mut Context<Self>) {
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            let line_mode = s.line_mode;
            s.move_with(|map, selection| {
                let cursor = if selection.is_empty() && !line_mode {
                    movement::right(map, selection.end)
                } else {
                    selection.end
                };
                selection.collapse_to(cursor, SelectionGoal::None)
            });
        })
    }

    pub fn select_right(&mut self, _: &SelectRight, window: &mut Window, cx: &mut Context<Self>) {
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_heads_with(|map, head, _| (movement::right(map, head), SelectionGoal::None));
        })
    }

    pub fn move_up(&mut self, _: &MoveUp, window: &mut Window, cx: &mut Context<Self>) {
        if self.take_rename(true, window, cx).is_some() {
            return;
        }

        if matches!(self.mode, EditorMode::SingleLine { .. }) {
            cx.propagate();
            return;
        }

        let text_layout_details = &self.text_layout_details(window);
        let selection_count = self.selections.count();
        let first_selection = self.selections.first_anchor();

        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            let line_mode = s.line_mode;
            s.move_with(|map, selection| {
                if !selection.is_empty() && !line_mode {
                    selection.goal = SelectionGoal::None;
                }
                let (cursor, goal) = movement::up(
                    map,
                    selection.start,
                    selection.goal,
                    false,
                    text_layout_details,
                );
                selection.collapse_to(cursor, goal);
            });
        });

        if selection_count == 1 && first_selection.range() == self.selections.first_anchor().range()
        {
            cx.propagate();
        }
    }

    pub fn move_up_by_lines(
        &mut self,
        action: &MoveUpByLines,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.take_rename(true, window, cx).is_some() {
            return;
        }

        if matches!(self.mode, EditorMode::SingleLine { .. }) {
            cx.propagate();
            return;
        }

        let text_layout_details = &self.text_layout_details(window);

        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            let line_mode = s.line_mode;
            s.move_with(|map, selection| {
                if !selection.is_empty() && !line_mode {
                    selection.goal = SelectionGoal::None;
                }
                let (cursor, goal) = movement::up_by_rows(
                    map,
                    selection.start,
                    action.lines,
                    selection.goal,
                    false,
                    text_layout_details,
                );
                selection.collapse_to(cursor, goal);
            });
        })
    }

    pub fn move_down_by_lines(
        &mut self,
        action: &MoveDownByLines,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.take_rename(true, window, cx).is_some() {
            return;
        }

        if matches!(self.mode, EditorMode::SingleLine { .. }) {
            cx.propagate();
            return;
        }

        let text_layout_details = &self.text_layout_details(window);

        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            let line_mode = s.line_mode;
            s.move_with(|map, selection| {
                if !selection.is_empty() && !line_mode {
                    selection.goal = SelectionGoal::None;
                }
                let (cursor, goal) = movement::down_by_rows(
                    map,
                    selection.start,
                    action.lines,
                    selection.goal,
                    false,
                    text_layout_details,
                );
                selection.collapse_to(cursor, goal);
            });
        })
    }

    pub fn select_down_by_lines(
        &mut self,
        action: &SelectDownByLines,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text_layout_details = &self.text_layout_details(window);
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_heads_with(|map, head, goal| {
                movement::down_by_rows(map, head, action.lines, goal, false, text_layout_details)
            })
        })
    }

    pub fn select_up_by_lines(
        &mut self,
        action: &SelectUpByLines,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text_layout_details = &self.text_layout_details(window);
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_heads_with(|map, head, goal| {
                movement::up_by_rows(map, head, action.lines, goal, false, text_layout_details)
            })
        })
    }

    pub fn select_page_up(
        &mut self,
        _: &SelectPageUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row_count) = self.visible_row_count() else {
            return;
        };

        let text_layout_details = &self.text_layout_details(window);

        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_heads_with(|map, head, goal| {
                movement::up_by_rows(map, head, row_count, goal, false, text_layout_details)
            })
        })
    }

    pub fn move_page_up(
        &mut self,
        action: &MovePageUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.take_rename(true, window, cx).is_some() {
            return;
        }

        if self
            .context_menu
            .borrow_mut()
            .as_mut()
            .map(|menu| menu.select_first(self.completion_provider.as_deref(), cx))
            .unwrap_or(false)
        {
            return;
        }

        if matches!(self.mode, EditorMode::SingleLine { .. }) {
            cx.propagate();
            return;
        }

        let Some(row_count) = self.visible_row_count() else {
            return;
        };

        let autoscroll = if action.center_cursor {
            Autoscroll::center()
        } else {
            Autoscroll::fit()
        };

        let text_layout_details = &self.text_layout_details(window);

        self.change_selections(Some(autoscroll), window, cx, |s| {
            let line_mode = s.line_mode;
            s.move_with(|map, selection| {
                if !selection.is_empty() && !line_mode {
                    selection.goal = SelectionGoal::None;
                }
                let (cursor, goal) = movement::up_by_rows(
                    map,
                    selection.end,
                    row_count,
                    selection.goal,
                    false,
                    text_layout_details,
                );
                selection.collapse_to(cursor, goal);
            });
        });
    }

    pub fn select_up(&mut self, _: &SelectUp, window: &mut Window, cx: &mut Context<Self>) {
        let text_layout_details = &self.text_layout_details(window);
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_heads_with(|map, head, goal| {
                movement::up(map, head, goal, false, text_layout_details)
            })
        })
    }

    pub fn move_down(&mut self, _: &MoveDown, window: &mut Window, cx: &mut Context<Self>) {
        self.take_rename(true, window, cx);

        if matches!(self.mode, EditorMode::SingleLine { .. }) {
            cx.propagate();
            return;
        }

        let text_layout_details = &self.text_layout_details(window);
        let selection_count = self.selections.count();
        let first_selection = self.selections.first_anchor();

        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            let line_mode = s.line_mode;
            s.move_with(|map, selection| {
                if !selection.is_empty() && !line_mode {
                    selection.goal = SelectionGoal::None;
                }
                let (cursor, goal) = movement::down(
                    map,
                    selection.end,
                    selection.goal,
                    false,
                    text_layout_details,
                );
                selection.collapse_to(cursor, goal);
            });
        });

        if selection_count == 1 && first_selection.range() == self.selections.first_anchor().range()
        {
            cx.propagate();
        }
    }

    pub fn select_page_down(
        &mut self,
        _: &SelectPageDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row_count) = self.visible_row_count() else {
            return;
        };

        let text_layout_details = &self.text_layout_details(window);

        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_heads_with(|map, head, goal| {
                movement::down_by_rows(map, head, row_count, goal, false, text_layout_details)
            })
        })
    }

    pub fn move_page_down(
        &mut self,
        action: &MovePageDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.take_rename(true, window, cx).is_some() {
            return;
        }

        if self
            .context_menu
            .borrow_mut()
            .as_mut()
            .map(|menu| menu.select_last(self.completion_provider.as_deref(), cx))
            .unwrap_or(false)
        {
            return;
        }

        if matches!(self.mode, EditorMode::SingleLine { .. }) {
            cx.propagate();
            return;
        }

        let Some(row_count) = self.visible_row_count() else {
            return;
        };

        let autoscroll = if action.center_cursor {
            Autoscroll::center()
        } else {
            Autoscroll::fit()
        };

        let text_layout_details = &self.text_layout_details(window);
        self.change_selections(Some(autoscroll), window, cx, |s| {
            let line_mode = s.line_mode;
            s.move_with(|map, selection| {
                if !selection.is_empty() && !line_mode {
                    selection.goal = SelectionGoal::None;
                }
                let (cursor, goal) = movement::down_by_rows(
                    map,
                    selection.end,
                    row_count,
                    selection.goal,
                    false,
                    text_layout_details,
                );
                selection.collapse_to(cursor, goal);
            });
        });
    }

    pub fn select_down(&mut self, _: &SelectDown, window: &mut Window, cx: &mut Context<Self>) {
        let text_layout_details = &self.text_layout_details(window);
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_heads_with(|map, head, goal| {
                movement::down(map, head, goal, false, text_layout_details)
            })
        });
    }

    pub fn context_menu_first(
        &mut self,
        _: &ContextMenuFirst,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(context_menu) = self.context_menu.borrow_mut().as_mut() {
            context_menu.select_first(self.completion_provider.as_deref(), cx);
        }
    }

    pub fn context_menu_prev(
        &mut self,
        _: &ContextMenuPrev,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(context_menu) = self.context_menu.borrow_mut().as_mut() {
            context_menu.select_prev(self.completion_provider.as_deref(), cx);
        }
    }

    pub fn context_menu_next(
        &mut self,
        _: &ContextMenuNext,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(context_menu) = self.context_menu.borrow_mut().as_mut() {
            context_menu.select_next(self.completion_provider.as_deref(), cx);
        }
    }

    pub fn context_menu_last(
        &mut self,
        _: &ContextMenuLast,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(context_menu) = self.context_menu.borrow_mut().as_mut() {
            context_menu.select_last(self.completion_provider.as_deref(), cx);
        }
    }

    pub fn move_to_previous_word_start(
        &mut self,
        _: &MoveToPreviousWordStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_cursors_with(|map, head, _| {
                (
                    movement::previous_word_start(map, head),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn move_to_previous_subword_start(
        &mut self,
        _: &MoveToPreviousSubwordStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_cursors_with(|map, head, _| {
                (
                    movement::previous_subword_start(map, head),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn select_to_previous_word_start(
        &mut self,
        _: &SelectToPreviousWordStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_heads_with(|map, head, _| {
                (
                    movement::previous_word_start(map, head),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn select_to_previous_subword_start(
        &mut self,
        _: &SelectToPreviousSubwordStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_heads_with(|map, head, _| {
                (
                    movement::previous_subword_start(map, head),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn delete_to_previous_word_start(
        &mut self,
        action: &DeleteToPreviousWordStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.transact(window, cx, |this, window, cx| {
            this.select_autoclose_pair(window, cx);
            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                let line_mode = s.line_mode;
                s.move_with(|map, selection| {
                    if selection.is_empty() && !line_mode {
                        let cursor = if action.ignore_newlines {
                            movement::previous_word_start(map, selection.head())
                        } else {
                            movement::previous_word_start_or_newline(map, selection.head())
                        };
                        selection.set_head(cursor, SelectionGoal::None);
                    }
                });
            });
            this.insert("", window, cx);
        });
    }

    pub fn delete_to_previous_subword_start(
        &mut self,
        _: &DeleteToPreviousSubwordStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.transact(window, cx, |this, window, cx| {
            this.select_autoclose_pair(window, cx);
            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                let line_mode = s.line_mode;
                s.move_with(|map, selection| {
                    if selection.is_empty() && !line_mode {
                        let cursor = movement::previous_subword_start(map, selection.head());
                        selection.set_head(cursor, SelectionGoal::None);
                    }
                });
            });
            this.insert("", window, cx);
        });
    }

    pub fn move_to_next_word_end(
        &mut self,
        _: &MoveToNextWordEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_cursors_with(|map, head, _| {
                (movement::next_word_end(map, head), SelectionGoal::None)
            });
        })
    }

    pub fn move_to_next_subword_end(
        &mut self,
        _: &MoveToNextSubwordEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_cursors_with(|map, head, _| {
                (movement::next_subword_end(map, head), SelectionGoal::None)
            });
        })
    }

    pub fn select_to_next_word_end(
        &mut self,
        _: &SelectToNextWordEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_heads_with(|map, head, _| {
                (movement::next_word_end(map, head), SelectionGoal::None)
            });
        })
    }

    pub fn select_to_next_subword_end(
        &mut self,
        _: &SelectToNextSubwordEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_heads_with(|map, head, _| {
                (movement::next_subword_end(map, head), SelectionGoal::None)
            });
        })
    }

    pub fn delete_to_next_word_end(
        &mut self,
        action: &DeleteToNextWordEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.transact(window, cx, |this, window, cx| {
            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                let line_mode = s.line_mode;
                s.move_with(|map, selection| {
                    if selection.is_empty() && !line_mode {
                        let cursor = if action.ignore_newlines {
                            movement::next_word_end(map, selection.head())
                        } else {
                            movement::next_word_end_or_newline(map, selection.head())
                        };
                        selection.set_head(cursor, SelectionGoal::None);
                    }
                });
            });
            this.insert("", window, cx);
        });
    }

    pub fn delete_to_next_subword_end(
        &mut self,
        _: &DeleteToNextSubwordEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.transact(window, cx, |this, window, cx| {
            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.move_with(|map, selection| {
                    if selection.is_empty() {
                        let cursor = movement::next_subword_end(map, selection.head());
                        selection.set_head(cursor, SelectionGoal::None);
                    }
                });
            });
            this.insert("", window, cx);
        });
    }

    pub fn move_to_beginning_of_line(
        &mut self,
        action: &MoveToBeginningOfLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_cursors_with(|map, head, _| {
                (
                    movement::indented_line_beginning(map, head, action.stop_at_soft_wraps),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn select_to_beginning_of_line(
        &mut self,
        action: &SelectToBeginningOfLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_heads_with(|map, head, _| {
                (
                    movement::indented_line_beginning(map, head, action.stop_at_soft_wraps),
                    SelectionGoal::None,
                )
            });
        });
    }

    pub fn delete_to_beginning_of_line(
        &mut self,
        _: &DeleteToBeginningOfLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.transact(window, cx, |this, window, cx| {
            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.move_with(|_, selection| {
                    selection.reversed = true;
                });
            });

            this.select_to_beginning_of_line(
                &SelectToBeginningOfLine {
                    stop_at_soft_wraps: false,
                },
                window,
                cx,
            );
            this.backspace(&Backspace, window, cx);
        });
    }

    pub fn move_to_end_of_line(
        &mut self,
        action: &MoveToEndOfLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_cursors_with(|map, head, _| {
                (
                    movement::line_end(map, head, action.stop_at_soft_wraps),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn select_to_end_of_line(
        &mut self,
        action: &SelectToEndOfLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_heads_with(|map, head, _| {
                (
                    movement::line_end(map, head, action.stop_at_soft_wraps),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn delete_to_end_of_line(
        &mut self,
        _: &DeleteToEndOfLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.transact(window, cx, |this, window, cx| {
            this.select_to_end_of_line(
                &SelectToEndOfLine {
                    stop_at_soft_wraps: false,
                },
                window,
                cx,
            );
            this.delete(&Delete, window, cx);
        });
    }

    pub fn cut_to_end_of_line(
        &mut self,
        _: &CutToEndOfLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.transact(window, cx, |this, window, cx| {
            this.select_to_end_of_line(
                &SelectToEndOfLine {
                    stop_at_soft_wraps: false,
                },
                window,
                cx,
            );
            this.cut(&Cut, window, cx);
        });
    }

    pub fn move_to_start_of_paragraph(
        &mut self,
        _: &MoveToStartOfParagraph,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.mode, EditorMode::SingleLine { .. }) {
            cx.propagate();
            return;
        }

        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_with(|map, selection| {
                selection.collapse_to(
                    movement::start_of_paragraph(map, selection.head(), 1),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn move_to_end_of_paragraph(
        &mut self,
        _: &MoveToEndOfParagraph,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.mode, EditorMode::SingleLine { .. }) {
            cx.propagate();
            return;
        }

        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_with(|map, selection| {
                selection.collapse_to(
                    movement::end_of_paragraph(map, selection.head(), 1),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn select_to_start_of_paragraph(
        &mut self,
        _: &SelectToStartOfParagraph,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.mode, EditorMode::SingleLine { .. }) {
            cx.propagate();
            return;
        }

        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_heads_with(|map, head, _| {
                (
                    movement::start_of_paragraph(map, head, 1),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn select_to_end_of_paragraph(
        &mut self,
        _: &SelectToEndOfParagraph,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.mode, EditorMode::SingleLine { .. }) {
            cx.propagate();
            return;
        }

        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_heads_with(|map, head, _| {
                (
                    movement::end_of_paragraph(map, head, 1),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn move_to_beginning(
        &mut self,
        _: &MoveToBeginning,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.mode, EditorMode::SingleLine { .. }) {
            cx.propagate();
            return;
        }

        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.select_ranges(vec![0..0]);
        });
    }

    pub fn select_to_beginning(
        &mut self,
        _: &SelectToBeginning,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut selection = self.selections.last::<Point>(cx);
        selection.set_head(Point::zero(), SelectionGoal::None);

        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.select(vec![selection]);
        });
    }

    pub fn move_to_end(&mut self, _: &MoveToEnd, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.mode, EditorMode::SingleLine { .. }) {
            cx.propagate();
            return;
        }

        let cursor = self.buffer.read(cx).read(cx).len();
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.select_ranges(vec![cursor..cursor])
        });
    }

    pub fn set_nav_history(&mut self, nav_history: Option<ItemNavHistory>) {
        self.nav_history = nav_history;
    }

    pub fn nav_history(&self) -> Option<&ItemNavHistory> {
        self.nav_history.as_ref()
    }

    fn push_to_nav_history(
        &mut self,
        cursor_anchor: Anchor,
        new_position: Option<Point>,
        cx: &mut Context<Self>,
    ) {
        if let Some(nav_history) = self.nav_history.as_mut() {
            let buffer = self.buffer.read(cx).read(cx);
            let cursor_position = cursor_anchor.to_point(&buffer);
            let scroll_state = self.scroll_manager.anchor();
            let scroll_top_row = scroll_state.top_row(&buffer);
            drop(buffer);

            if let Some(new_position) = new_position {
                let row_delta = (new_position.row as i64 - cursor_position.row as i64).abs();
                if row_delta < MIN_NAVIGATION_HISTORY_ROW_DELTA {
                    return;
                }
            }

            nav_history.push(
                Some(NavigationData {
                    cursor_anchor,
                    cursor_position,
                    scroll_anchor: scroll_state,
                    scroll_top_row,
                }),
                cx,
            );
        }
    }

    pub fn select_to_end(&mut self, _: &SelectToEnd, window: &mut Window, cx: &mut Context<Self>) {
        let buffer = self.buffer.read(cx).snapshot(cx);
        let mut selection = self.selections.first::<usize>(cx);
        selection.set_head(buffer.len(), SelectionGoal::None);
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.select(vec![selection]);
        });
    }

    pub fn select_all(&mut self, _: &SelectAll, window: &mut Window, cx: &mut Context<Self>) {
        let end = self.buffer.read(cx).read(cx).len();
        self.change_selections(None, window, cx, |s| {
            s.select_ranges(vec![0..end]);
        });
    }

    pub fn select_line(&mut self, _: &SelectLine, window: &mut Window, cx: &mut Context<Self>) {
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let mut selections = self.selections.all::<Point>(cx);
        let max_point = display_map.buffer_snapshot.max_point();
        for selection in &mut selections {
            let rows = selection.spanned_rows(true, &display_map);
            selection.start = Point::new(rows.start.0, 0);
            selection.end = cmp::min(max_point, Point::new(rows.end.0, 0));
            selection.reversed = false;
        }
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.select(selections);
        });
    }

    pub fn split_selection_into_lines(
        &mut self,
        _: &SplitSelectionIntoLines,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut to_unfold = Vec::new();
        let mut new_selection_ranges = Vec::new();
        {
            let selections = self.selections.all::<Point>(cx);
            let buffer = self.buffer.read(cx).read(cx);
            for selection in selections {
                for row in selection.start.row..selection.end.row {
                    let cursor = Point::new(row, buffer.line_len(MultiBufferRow(row)));
                    new_selection_ranges.push(cursor..cursor);
                }
                new_selection_ranges.push(selection.end..selection.end);
                to_unfold.push(selection.start..selection.end);
            }
        }
        self.unfold_ranges(&to_unfold, true, true, cx);
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.select_ranges(new_selection_ranges);
        });
    }

    pub fn add_selection_above(
        &mut self,
        _: &AddSelectionAbove,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_selection(true, window, cx);
    }

    pub fn add_selection_below(
        &mut self,
        _: &AddSelectionBelow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_selection(false, window, cx);
    }

    fn add_selection(&mut self, above: bool, window: &mut Window, cx: &mut Context<Self>) {
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let mut selections = self.selections.all::<Point>(cx);
        let text_layout_details = self.text_layout_details(window);
        let mut state = self.add_selections_state.take().unwrap_or_else(|| {
            let oldest_selection = selections.iter().min_by_key(|s| s.id).unwrap().clone();
            let range = oldest_selection.display_range(&display_map).sorted();

            let start_x = display_map.x_for_display_point(range.start, &text_layout_details);
            let end_x = display_map.x_for_display_point(range.end, &text_layout_details);
            let positions = start_x.min(end_x)..start_x.max(end_x);

            selections.clear();
            let mut stack = Vec::new();
            for row in range.start.row().0..=range.end.row().0 {
                if let Some(selection) = self.selections.build_columnar_selection(
                    &display_map,
                    DisplayRow(row),
                    &positions,
                    oldest_selection.reversed,
                    &text_layout_details,
                ) {
                    stack.push(selection.id);
                    selections.push(selection);
                }
            }

            if above {
                stack.reverse();
            }

            AddSelectionsState { above, stack }
        });

        let last_added_selection = *state.stack.last().unwrap();
        let mut new_selections = Vec::new();
        if above == state.above {
            let end_row = if above {
                DisplayRow(0)
            } else {
                display_map.max_point().row()
            };

            'outer: for selection in selections {
                if selection.id == last_added_selection {
                    let range = selection.display_range(&display_map).sorted();
                    debug_assert_eq!(range.start.row(), range.end.row());
                    let mut row = range.start.row();
                    let positions =
                        if let SelectionGoal::HorizontalRange { start, end } = selection.goal {
                            px(start)..px(end)
                        } else {
                            let start_x =
                                display_map.x_for_display_point(range.start, &text_layout_details);
                            let end_x =
                                display_map.x_for_display_point(range.end, &text_layout_details);
                            start_x.min(end_x)..start_x.max(end_x)
                        };

                    while row != end_row {
                        if above {
                            row.0 -= 1;
                        } else {
                            row.0 += 1;
                        }

                        if let Some(new_selection) = self.selections.build_columnar_selection(
                            &display_map,
                            row,
                            &positions,
                            selection.reversed,
                            &text_layout_details,
                        ) {
                            state.stack.push(new_selection.id);
                            if above {
                                new_selections.push(new_selection);
                                new_selections.push(selection);
                            } else {
                                new_selections.push(selection);
                                new_selections.push(new_selection);
                            }

                            continue 'outer;
                        }
                    }
                }

                new_selections.push(selection);
            }
        } else {
            new_selections = selections;
            new_selections.retain(|s| s.id != last_added_selection);
            state.stack.pop();
        }

        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.select(new_selections);
        });
        if state.stack.len() > 1 {
            self.add_selections_state = Some(state);
        }
    }

    pub fn select_next_match_internal(
        &mut self,
        display_map: &DisplaySnapshot,
        replace_newest: bool,
        autoscroll: Option<Autoscroll>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        fn select_next_match_ranges(
            this: &mut Editor,
            range: Range<usize>,
            replace_newest: bool,
            auto_scroll: Option<Autoscroll>,
            window: &mut Window,
            cx: &mut Context<Editor>,
        ) {
            this.unfold_ranges(&[range.clone()], false, true, cx);
            this.change_selections(auto_scroll, window, cx, |s| {
                if replace_newest {
                    s.delete(s.newest_anchor().id);
                }
                s.insert_range(range.clone());
            });
        }

        let buffer = &display_map.buffer_snapshot;
        let mut selections = self.selections.all::<usize>(cx);
        if let Some(mut select_next_state) = self.select_next_state.take() {
            let query = &select_next_state.query;
            if !select_next_state.done {
                let first_selection = selections.iter().min_by_key(|s| s.id).unwrap();
                let last_selection = selections.iter().max_by_key(|s| s.id).unwrap();
                let mut next_selected_range = None;

                let bytes_after_last_selection =
                    buffer.bytes_in_range(last_selection.end..buffer.len());
                let bytes_before_first_selection = buffer.bytes_in_range(0..first_selection.start);
                let query_matches = query
                    .stream_find_iter(bytes_after_last_selection)
                    .map(|result| (last_selection.end, result))
                    .chain(
                        query
                            .stream_find_iter(bytes_before_first_selection)
                            .map(|result| (0, result)),
                    );

                for (start_offset, query_match) in query_matches {
                    let query_match = query_match.unwrap(); // can only fail due to I/O
                    let offset_range =
                        start_offset + query_match.start()..start_offset + query_match.end();
                    let display_range = offset_range.start.to_display_point(display_map)
                        ..offset_range.end.to_display_point(display_map);

                    if !select_next_state.wordwise
                        || (!movement::is_inside_word(display_map, display_range.start)
                            && !movement::is_inside_word(display_map, display_range.end))
                    {
                        // TODO: This is n^2, because we might check all the selections
                        if !selections
                            .iter()
                            .any(|selection| selection.range().overlaps(&offset_range))
                        {
                            next_selected_range = Some(offset_range);
                            break;
                        }
                    }
                }

                if let Some(next_selected_range) = next_selected_range {
                    select_next_match_ranges(
                        self,
                        next_selected_range,
                        replace_newest,
                        autoscroll,
                        window,
                        cx,
                    );
                } else {
                    select_next_state.done = true;
                }
            }

            self.select_next_state = Some(select_next_state);
        } else {
            let mut only_carets = true;
            let mut same_text_selected = true;
            let mut selected_text = None;

            let mut selections_iter = selections.iter().peekable();
            while let Some(selection) = selections_iter.next() {
                if selection.start != selection.end {
                    only_carets = false;
                }

                if same_text_selected {
                    if selected_text.is_none() {
                        selected_text =
                            Some(buffer.text_for_range(selection.range()).collect::<String>());
                    }

                    if let Some(next_selection) = selections_iter.peek() {
                        if next_selection.range().len() == selection.range().len() {
                            let next_selected_text = buffer
                                .text_for_range(next_selection.range())
                                .collect::<String>();
                            if Some(next_selected_text) != selected_text {
                                same_text_selected = false;
                                selected_text = None;
                            }
                        } else {
                            same_text_selected = false;
                            selected_text = None;
                        }
                    }
                }
            }

            if only_carets {
                for selection in &mut selections {
                    let word_range = movement::surrounding_word(
                        display_map,
                        selection.start.to_display_point(display_map),
                    );
                    selection.start = word_range.start.to_offset(display_map, Bias::Left);
                    selection.end = word_range.end.to_offset(display_map, Bias::Left);
                    selection.goal = SelectionGoal::None;
                    selection.reversed = false;
                    select_next_match_ranges(
                        self,
                        selection.start..selection.end,
                        replace_newest,
                        autoscroll,
                        window,
                        cx,
                    );
                }

                if selections.len() == 1 {
                    let selection = selections
                        .last()
                        .expect("ensured that there's only one selection");
                    let query = buffer
                        .text_for_range(selection.start..selection.end)
                        .collect::<String>();
                    let is_empty = query.is_empty();
                    let select_state = SelectNextState {
                        query: AhoCorasick::new(&[query])?,
                        wordwise: true,
                        done: is_empty,
                    };
                    self.select_next_state = Some(select_state);
                } else {
                    self.select_next_state = None;
                }
            } else if let Some(selected_text) = selected_text {
                self.select_next_state = Some(SelectNextState {
                    query: AhoCorasick::new(&[selected_text])?,
                    wordwise: false,
                    done: false,
                });
                self.select_next_match_internal(
                    display_map,
                    replace_newest,
                    autoscroll,
                    window,
                    cx,
                )?;
            }
        }
        Ok(())
    }

    pub fn select_all_matches(
        &mut self,
        _action: &SelectAllMatches,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.push_to_selection_history();
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));

        self.select_next_match_internal(&display_map, false, None, window, cx)?;
        let Some(select_next_state) = self.select_next_state.as_mut() else {
            return Ok(());
        };
        if select_next_state.done {
            return Ok(());
        }

        let mut new_selections = self.selections.all::<usize>(cx);

        let buffer = &display_map.buffer_snapshot;
        let query_matches = select_next_state
            .query
            .stream_find_iter(buffer.bytes_in_range(0..buffer.len()));

        for query_match in query_matches {
            let query_match = query_match.unwrap(); // can only fail due to I/O
            let offset_range = query_match.start()..query_match.end();
            let display_range = offset_range.start.to_display_point(&display_map)
                ..offset_range.end.to_display_point(&display_map);

            if !select_next_state.wordwise
                || (!movement::is_inside_word(&display_map, display_range.start)
                    && !movement::is_inside_word(&display_map, display_range.end))
            {
                self.selections.change_with(cx, |selections| {
                    new_selections.push(Selection {
                        id: selections.new_selection_id(),
                        start: offset_range.start,
                        end: offset_range.end,
                        reversed: false,
                        goal: SelectionGoal::None,
                    });
                });
            }
        }

        new_selections.sort_by_key(|selection| selection.start);
        let mut ix = 0;
        while ix + 1 < new_selections.len() {
            let current_selection = &new_selections[ix];
            let next_selection = &new_selections[ix + 1];
            if current_selection.range().overlaps(&next_selection.range()) {
                if current_selection.id < next_selection.id {
                    new_selections.remove(ix + 1);
                } else {
                    new_selections.remove(ix);
                }
            } else {
                ix += 1;
            }
        }

        let reversed = self.selections.oldest::<usize>(cx).reversed;

        for selection in new_selections.iter_mut() {
            selection.reversed = reversed;
        }

        select_next_state.done = true;
        self.unfold_ranges(
            &new_selections
                .iter()
                .map(|selection| selection.range())
                .collect::<Vec<_>>(),
            false,
            false,
            cx,
        );
        self.change_selections(Some(Autoscroll::fit()), window, cx, |selections| {
            selections.select(new_selections)
        });

        Ok(())
    }

    pub fn select_next(
        &mut self,
        action: &SelectNext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.push_to_selection_history();
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        self.select_next_match_internal(
            &display_map,
            action.replace_newest,
            Some(Autoscroll::newest()),
            window,
            cx,
        )?;
        Ok(())
    }

    pub fn select_previous(
        &mut self,
        action: &SelectPrevious,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.push_to_selection_history();
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let buffer = &display_map.buffer_snapshot;
        let mut selections = self.selections.all::<usize>(cx);
        if let Some(mut select_prev_state) = self.select_prev_state.take() {
            let query = &select_prev_state.query;
            if !select_prev_state.done {
                let first_selection = selections.iter().min_by_key(|s| s.id).unwrap();
                let last_selection = selections.iter().max_by_key(|s| s.id).unwrap();
                let mut next_selected_range = None;
                // When we're iterating matches backwards, the oldest match will actually be the furthest one in the buffer.
                let bytes_before_last_selection =
                    buffer.reversed_bytes_in_range(0..last_selection.start);
                let bytes_after_first_selection =
                    buffer.reversed_bytes_in_range(first_selection.end..buffer.len());
                let query_matches = query
                    .stream_find_iter(bytes_before_last_selection)
                    .map(|result| (last_selection.start, result))
                    .chain(
                        query
                            .stream_find_iter(bytes_after_first_selection)
                            .map(|result| (buffer.len(), result)),
                    );
                for (end_offset, query_match) in query_matches {
                    let query_match = query_match.unwrap(); // can only fail due to I/O
                    let offset_range =
                        end_offset - query_match.end()..end_offset - query_match.start();
                    let display_range = offset_range.start.to_display_point(&display_map)
                        ..offset_range.end.to_display_point(&display_map);

                    if !select_prev_state.wordwise
                        || (!movement::is_inside_word(&display_map, display_range.start)
                            && !movement::is_inside_word(&display_map, display_range.end))
                    {
                        next_selected_range = Some(offset_range);
                        break;
                    }
                }

                if let Some(next_selected_range) = next_selected_range {
                    self.unfold_ranges(&[next_selected_range.clone()], false, true, cx);
                    self.change_selections(Some(Autoscroll::newest()), window, cx, |s| {
                        if action.replace_newest {
                            s.delete(s.newest_anchor().id);
                        }
                        s.insert_range(next_selected_range);
                    });
                } else {
                    select_prev_state.done = true;
                }
            }

            self.select_prev_state = Some(select_prev_state);
        } else {
            let mut only_carets = true;
            let mut same_text_selected = true;
            let mut selected_text = None;

            let mut selections_iter = selections.iter().peekable();
            while let Some(selection) = selections_iter.next() {
                if selection.start != selection.end {
                    only_carets = false;
                }

                if same_text_selected {
                    if selected_text.is_none() {
                        selected_text =
                            Some(buffer.text_for_range(selection.range()).collect::<String>());
                    }

                    if let Some(next_selection) = selections_iter.peek() {
                        if next_selection.range().len() == selection.range().len() {
                            let next_selected_text = buffer
                                .text_for_range(next_selection.range())
                                .collect::<String>();
                            if Some(next_selected_text) != selected_text {
                                same_text_selected = false;
                                selected_text = None;
                            }
                        } else {
                            same_text_selected = false;
                            selected_text = None;
                        }
                    }
                }
            }

            if only_carets {
                for selection in &mut selections {
                    let word_range = movement::surrounding_word(
                        &display_map,
                        selection.start.to_display_point(&display_map),
                    );
                    selection.start = word_range.start.to_offset(&display_map, Bias::Left);
                    selection.end = word_range.end.to_offset(&display_map, Bias::Left);
                    selection.goal = SelectionGoal::None;
                    selection.reversed = false;
                }
                if selections.len() == 1 {
                    let selection = selections
                        .last()
                        .expect("ensured that there's only one selection");
                    let query = buffer
                        .text_for_range(selection.start..selection.end)
                        .collect::<String>();
                    let is_empty = query.is_empty();
                    let select_state = SelectNextState {
                        query: AhoCorasick::new(&[query.chars().rev().collect::<String>()])?,
                        wordwise: true,
                        done: is_empty,
                    };
                    self.select_prev_state = Some(select_state);
                } else {
                    self.select_prev_state = None;
                }

                self.unfold_ranges(
                    &selections.iter().map(|s| s.range()).collect::<Vec<_>>(),
                    false,
                    true,
                    cx,
                );
                self.change_selections(Some(Autoscroll::newest()), window, cx, |s| {
                    s.select(selections);
                });
            } else if let Some(selected_text) = selected_text {
                self.select_prev_state = Some(SelectNextState {
                    query: AhoCorasick::new(&[selected_text.chars().rev().collect::<String>()])?,
                    wordwise: false,
                    done: false,
                });
                self.select_previous(action, window, cx)?;
            }
        }
        Ok(())
    }

    pub fn toggle_comments(
        &mut self,
        action: &ToggleComments,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }
        let text_layout_details = &self.text_layout_details(window);
        self.transact(window, cx, |this, window, cx| {
            let mut selections = this.selections.all::<MultiBufferPoint>(cx);
            let mut edits = Vec::new();
            let mut selection_edit_ranges = Vec::new();
            let mut last_toggled_row = None;
            let snapshot = this.buffer.read(cx).read(cx);
            let empty_str: Arc<str> = Arc::default();
            let mut suffixes_inserted = Vec::new();
            let ignore_indent = action.ignore_indent;

            fn comment_prefix_range(
                snapshot: &MultiBufferSnapshot,
                row: MultiBufferRow,
                comment_prefix: &str,
                comment_prefix_whitespace: &str,
                ignore_indent: bool,
            ) -> Range<Point> {
                let indent_size = if ignore_indent {
                    0
                } else {
                    snapshot.indent_size_for_line(row).len
                };

                let start = Point::new(row.0, indent_size);

                let mut line_bytes = snapshot
                    .bytes_in_range(start..snapshot.max_point())
                    .flatten()
                    .copied();

                // If this line currently begins with the line comment prefix, then record
                // the range containing the prefix.
                if line_bytes
                    .by_ref()
                    .take(comment_prefix.len())
                    .eq(comment_prefix.bytes())
                {
                    // Include any whitespace that matches the comment prefix.
                    let matching_whitespace_len = line_bytes
                        .zip(comment_prefix_whitespace.bytes())
                        .take_while(|(a, b)| a == b)
                        .count() as u32;
                    let end = Point::new(
                        start.row,
                        start.column + comment_prefix.len() as u32 + matching_whitespace_len,
                    );
                    start..end
                } else {
                    start..start
                }
            }

            fn comment_suffix_range(
                snapshot: &MultiBufferSnapshot,
                row: MultiBufferRow,
                comment_suffix: &str,
                comment_suffix_has_leading_space: bool,
            ) -> Range<Point> {
                let end = Point::new(row.0, snapshot.line_len(row));
                let suffix_start_column = end.column.saturating_sub(comment_suffix.len() as u32);

                let mut line_end_bytes = snapshot
                    .bytes_in_range(Point::new(end.row, suffix_start_column.saturating_sub(1))..end)
                    .flatten()
                    .copied();

                let leading_space_len = if suffix_start_column > 0
                    && line_end_bytes.next() == Some(b' ')
                    && comment_suffix_has_leading_space
                {
                    1
                } else {
                    0
                };

                // If this line currently begins with the line comment prefix, then record
                // the range containing the prefix.
                if line_end_bytes.by_ref().eq(comment_suffix.bytes()) {
                    let start = Point::new(end.row, suffix_start_column - leading_space_len);
                    start..end
                } else {
                    end..end
                }
            }

            // TODO: Handle selections that cross excerpts
            for selection in &mut selections {
                let start_column = snapshot
                    .indent_size_for_line(MultiBufferRow(selection.start.row))
                    .len;
                let language = if let Some(language) =
                    snapshot.language_scope_at(Point::new(selection.start.row, start_column))
                {
                    language
                } else {
                    continue;
                };

                selection_edit_ranges.clear();

                // If multiple selections contain a given row, avoid processing that
                // row more than once.
                let mut start_row = MultiBufferRow(selection.start.row);
                if last_toggled_row == Some(start_row) {
                    start_row = start_row.next_row();
                }
                let end_row =
                    if selection.end.row > selection.start.row && selection.end.column == 0 {
                        MultiBufferRow(selection.end.row - 1)
                    } else {
                        MultiBufferRow(selection.end.row)
                    };
                last_toggled_row = Some(end_row);

                if start_row > end_row {
                    continue;
                }

                // If the language has line comments, toggle those.
                let mut full_comment_prefixes = language.line_comment_prefixes().to_vec();

                // If ignore_indent is set, trim spaces from the right side of all full_comment_prefixes
                if ignore_indent {
                    full_comment_prefixes = full_comment_prefixes
                        .into_iter()
                        .map(|s| Arc::from(s.trim_end()))
                        .collect();
                }

                if !full_comment_prefixes.is_empty() {
                    let first_prefix = full_comment_prefixes
                        .first()
                        .expect("prefixes is non-empty");
                    let prefix_trimmed_lengths = full_comment_prefixes
                        .iter()
                        .map(|p| p.trim_end_matches(' ').len())
                        .collect::<SmallVec<[usize; 4]>>();

                    let mut all_selection_lines_are_comments = true;

                    for row in start_row.0..=end_row.0 {
                        let row = MultiBufferRow(row);
                        if start_row < end_row && snapshot.is_line_blank(row) {
                            continue;
                        }

                        let prefix_range = full_comment_prefixes
                            .iter()
                            .zip(prefix_trimmed_lengths.iter().copied())
                            .map(|(prefix, trimmed_prefix_len)| {
                                comment_prefix_range(
                                    snapshot.deref(),
                                    row,
                                    &prefix[..trimmed_prefix_len],
                                    &prefix[trimmed_prefix_len..],
                                    ignore_indent,
                                )
                            })
                            .max_by_key(|range| range.end.column - range.start.column)
                            .expect("prefixes is non-empty");

                        if prefix_range.is_empty() {
                            all_selection_lines_are_comments = false;
                        }

                        selection_edit_ranges.push(prefix_range);
                    }

                    if all_selection_lines_are_comments {
                        edits.extend(
                            selection_edit_ranges
                                .iter()
                                .cloned()
                                .map(|range| (range, empty_str.clone())),
                        );
                    } else {
                        let min_column = selection_edit_ranges
                            .iter()
                            .map(|range| range.start.column)
                            .min()
                            .unwrap_or(0);
                        edits.extend(selection_edit_ranges.iter().map(|range| {
                            let position = Point::new(range.start.row, min_column);
                            (position..position, first_prefix.clone())
                        }));
                    }
                } else if let Some((full_comment_prefix, comment_suffix)) =
                    language.block_comment_delimiters()
                {
                    let comment_prefix = full_comment_prefix.trim_end_matches(' ');
                    let comment_prefix_whitespace = &full_comment_prefix[comment_prefix.len()..];
                    let prefix_range = comment_prefix_range(
                        snapshot.deref(),
                        start_row,
                        comment_prefix,
                        comment_prefix_whitespace,
                        ignore_indent,
                    );
                    let suffix_range = comment_suffix_range(
                        snapshot.deref(),
                        end_row,
                        comment_suffix.trim_start_matches(' '),
                        comment_suffix.starts_with(' '),
                    );

                    if prefix_range.is_empty() || suffix_range.is_empty() {
                        edits.push((
                            prefix_range.start..prefix_range.start,
                            full_comment_prefix.clone(),
                        ));
                        edits.push((suffix_range.end..suffix_range.end, comment_suffix.clone()));
                        suffixes_inserted.push((end_row, comment_suffix.len()));
                    } else {
                        edits.push((prefix_range, empty_str.clone()));
                        edits.push((suffix_range, empty_str.clone()));
                    }
                } else {
                    continue;
                }
            }

            drop(snapshot);
            this.buffer.update(cx, |buffer, cx| {
                buffer.edit(edits, None, cx);
            });

            // Adjust selections so that they end before any comment suffixes that
            // were inserted.
            let mut suffixes_inserted = suffixes_inserted.into_iter().peekable();
            let mut selections = this.selections.all::<Point>(cx);
            let snapshot = this.buffer.read(cx).read(cx);
            for selection in &mut selections {
                while let Some((row, suffix_len)) = suffixes_inserted.peek().copied() {
                    match row.cmp(&MultiBufferRow(selection.end.row)) {
                        Ordering::Less => {
                            suffixes_inserted.next();
                            continue;
                        }
                        Ordering::Greater => break,
                        Ordering::Equal => {
                            if selection.end.column == snapshot.line_len(row) {
                                if selection.is_empty() {
                                    selection.start.column -= suffix_len as u32;
                                }
                                selection.end.column -= suffix_len as u32;
                            }
                            break;
                        }
                    }
                }
            }

            drop(snapshot);
            this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select(selections)
            });

            let selections = this.selections.all::<Point>(cx);
            let selections_on_single_row = selections.windows(2).all(|selections| {
                selections[0].start.row == selections[1].start.row
                    && selections[0].end.row == selections[1].end.row
                    && selections[0].start.row == selections[0].end.row
            });
            let selections_selecting = selections
                .iter()
                .any(|selection| selection.start != selection.end);
            let advance_downwards = action.advance_downwards
                && selections_on_single_row
                && !selections_selecting
                && !matches!(this.mode, EditorMode::SingleLine { .. });

            if advance_downwards {
                let snapshot = this.buffer.read(cx).snapshot(cx);

                this.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                    s.move_cursors_with(|display_snapshot, display_point, _| {
                        let mut point = display_point.to_point(display_snapshot);
                        point.row += 1;
                        point = snapshot.clip_point(point, Bias::Left);
                        let display_point = point.to_display_point(display_snapshot);
                        let goal = SelectionGoal::HorizontalPosition(
                            display_snapshot
                                .x_for_display_point(display_point, text_layout_details)
                                .into(),
                        );
                        (display_point, goal)
                    })
                });
            }
        });
    }

    pub fn select_enclosing_symbol(
        &mut self,
        _: &SelectEnclosingSymbol,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let buffer = self.buffer.read(cx).snapshot(cx);
        let old_selections = self.selections.all::<usize>(cx).into_boxed_slice();

        fn update_selection(
            selection: &Selection<usize>,
            buffer_snap: &MultiBufferSnapshot,
        ) -> Option<Selection<usize>> {
            let cursor = selection.head();
            let (_buffer_id, symbols) = buffer_snap.symbols_containing(cursor, None)?;
            for symbol in symbols.iter().rev() {
                let start = symbol.range.start.to_offset(buffer_snap);
                let end = symbol.range.end.to_offset(buffer_snap);
                let new_range = start..end;
                if start < selection.start || end > selection.end {
                    return Some(Selection {
                        id: selection.id,
                        start: new_range.start,
                        end: new_range.end,
                        goal: SelectionGoal::None,
                        reversed: selection.reversed,
                    });
                }
            }
            None
        }

        let mut selected_larger_symbol = false;
        let new_selections = old_selections
            .iter()
            .map(|selection| match update_selection(selection, &buffer) {
                Some(new_selection) => {
                    if new_selection.range() != selection.range() {
                        selected_larger_symbol = true;
                    }
                    new_selection
                }
                None => selection.clone(),
            })
            .collect::<Vec<_>>();

        if selected_larger_symbol {
            self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select(new_selections);
            });
        }
    }

    pub fn select_larger_syntax_node(
        &mut self,
        _: &SelectLargerSyntaxNode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let buffer = self.buffer.read(cx).snapshot(cx);
        let old_selections = self.selections.all::<usize>(cx).into_boxed_slice();

        let mut stack = mem::take(&mut self.select_larger_syntax_node_stack);
        let mut selected_larger_node = false;
        let new_selections = old_selections
            .iter()
            .map(|selection| {
                let old_range = selection.start..selection.end;
                let mut new_range = old_range.clone();
                let mut new_node = None;
                while let Some((node, containing_range)) = buffer.syntax_ancestor(new_range.clone())
                {
                    new_node = Some(node);
                    new_range = containing_range;
                    if !display_map.intersects_fold(new_range.start)
                        && !display_map.intersects_fold(new_range.end)
                    {
                        break;
                    }
                }

                if let Some(node) = new_node {
                    // Log the ancestor, to support using this action as a way to explore TreeSitter
                    // nodes. Parent and grandparent are also logged because this operation will not
                    // visit nodes that have the same range as their parent.
                    log::info!("Node: {node:?}");
                    let parent = node.parent();
                    log::info!("Parent: {parent:?}");
                    let grandparent = parent.and_then(|x| x.parent());
                    log::info!("Grandparent: {grandparent:?}");
                }

                selected_larger_node |= new_range != old_range;
                Selection {
                    id: selection.id,
                    start: new_range.start,
                    end: new_range.end,
                    goal: SelectionGoal::None,
                    reversed: selection.reversed,
                }
            })
            .collect::<Vec<_>>();

        if selected_larger_node {
            stack.push(old_selections);
            self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select(new_selections);
            });
        }
        self.select_larger_syntax_node_stack = stack;
    }

    pub fn select_smaller_syntax_node(
        &mut self,
        _: &SelectSmallerSyntaxNode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut stack = mem::take(&mut self.select_larger_syntax_node_stack);
        if let Some(selections) = stack.pop() {
            self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select(selections.to_vec());
            });
        }
        self.select_larger_syntax_node_stack = stack;
    }

    fn refresh_runnables(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        if !EditorSettings::get_global(cx).gutter.runnables {
            self.clear_tasks();
            return Task::ready(());
        }
        let project = self.project.as_ref().map(Entity::downgrade);
        cx.spawn_in(window, |this, mut cx| async move {
            cx.background_executor().timer(UPDATE_DEBOUNCE).await;
            let Some(project) = project.and_then(|p| p.upgrade()) else {
                return;
            };
            let Ok(display_snapshot) = this.update(&mut cx, |this, cx| {
                this.display_map.update(cx, |map, cx| map.snapshot(cx))
            }) else {
                return;
            };

            let hide_runnables = project
                .update(&mut cx, |project, cx| {
                    // Do not display any test indicators in non-dev server remote projects.
                    project.is_via_collab() && project.ssh_connection_string(cx).is_none()
                })
                .unwrap_or(true);
            if hide_runnables {
                return;
            }
            let new_rows =
                cx.background_executor()
                    .spawn({
                        let snapshot = display_snapshot.clone();
                        async move {
                            Self::fetch_runnable_ranges(&snapshot, Anchor::min()..Anchor::max())
                        }
                    })
                    .await;

            let rows = Self::runnable_rows(project, display_snapshot, new_rows, cx.clone());
            this.update(&mut cx, |this, _| {
                this.clear_tasks();
                for (key, value) in rows {
                    this.insert_tasks(key, value);
                }
            })
            .ok();
        })
    }
    fn fetch_runnable_ranges(
        snapshot: &DisplaySnapshot,
        range: Range<Anchor>,
    ) -> Vec<language::RunnableRange> {
        snapshot.buffer_snapshot.runnable_ranges(range).collect()
    }

    fn runnable_rows(
        project: Entity<Project>,
        snapshot: DisplaySnapshot,
        runnable_ranges: Vec<RunnableRange>,
        mut cx: AsyncWindowContext,
    ) -> Vec<((BufferId, u32), RunnableTasks)> {
        runnable_ranges
            .into_iter()
            .filter_map(|mut runnable| {
                let tasks = cx
                    .update(|_, cx| Self::templates_with_tags(&project, &mut runnable.runnable, cx))
                    .ok()?;
                if tasks.is_empty() {
                    return None;
                }

                let point = runnable.run_range.start.to_point(&snapshot.buffer_snapshot);

                let row = snapshot
                    .buffer_snapshot
                    .buffer_line_for_row(MultiBufferRow(point.row))?
                    .1
                    .start
                    .row;

                let context_range =
                    BufferOffset(runnable.full_range.start)..BufferOffset(runnable.full_range.end);
                Some((
                    (runnable.buffer_id, row),
                    RunnableTasks {
                        templates: tasks,
                        offset: MultiBufferOffset(runnable.run_range.start),
                        context_range,
                        column: point.column,
                        extra_variables: runnable.extra_captures,
                    },
                ))
            })
            .collect()
    }

    fn templates_with_tags(
        project: &Entity<Project>,
        runnable: &mut Runnable,
        cx: &mut App,
    ) -> Vec<(TaskSourceKind, TaskTemplate)> {
        let (inventory, worktree_id, file) = project.read_with(cx, |project, cx| {
            let (worktree_id, file) = project
                .buffer_for_id(runnable.buffer, cx)
                .and_then(|buffer| buffer.read(cx).file())
                .map(|file| (file.worktree_id(cx), file.clone()))
                .unzip();

            (
                project.task_store().read(cx).task_inventory().cloned(),
                worktree_id,
                file,
            )
        });

        let tags = mem::take(&mut runnable.tags);
        let mut tags: Vec<_> = tags
            .into_iter()
            .flat_map(|tag| {
                let tag = tag.0.clone();
                inventory
                    .as_ref()
                    .into_iter()
                    .flat_map(|inventory| {
                        inventory.read(cx).list_tasks(
                            file.clone(),
                            Some(runnable.language.clone()),
                            worktree_id,
                            cx,
                        )
                    })
                    .filter(move |(_, template)| {
                        template.tags.iter().any(|source_tag| source_tag == &tag)
                    })
            })
            .sorted_by_key(|(kind, _)| kind.to_owned())
            .collect();
        if let Some((leading_tag_source, _)) = tags.first() {
            // Strongest source wins; if we have worktree tag binding, prefer that to
            // global and language bindings;
            // if we have a global binding, prefer that to language binding.
            let first_mismatch = tags
                .iter()
                .position(|(tag_source, _)| tag_source != leading_tag_source);
            if let Some(index) = first_mismatch {
                tags.truncate(index);
            }
        }

        tags
    }

    pub fn move_to_enclosing_bracket(
        &mut self,
        _: &MoveToEnclosingBracket,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
            s.move_offsets_with(|snapshot, selection| {
                let Some(enclosing_bracket_ranges) =
                    snapshot.enclosing_bracket_ranges(selection.start..selection.end)
                else {
                    return;
                };

                let mut best_length = usize::MAX;
                let mut best_inside = false;
                let mut best_in_bracket_range = false;
                let mut best_destination = None;
                for (open, close) in enclosing_bracket_ranges {
                    let close = close.to_inclusive();
                    let length = close.end() - open.start;
                    let inside = selection.start >= open.end && selection.end <= *close.start();
                    let in_bracket_range = open.to_inclusive().contains(&selection.head())
                        || close.contains(&selection.head());

                    // If best is next to a bracket and current isn't, skip
                    if !in_bracket_range && best_in_bracket_range {
                        continue;
                    }

                    // Prefer smaller lengths unless best is inside and current isn't
                    if length > best_length && (best_inside || !inside) {
                        continue;
                    }

                    best_length = length;
                    best_inside = inside;
                    best_in_bracket_range = in_bracket_range;
                    best_destination = Some(
                        if close.contains(&selection.start) && close.contains(&selection.end) {
                            if inside {
                                open.end
                            } else {
                                open.start
                            }
                        } else if inside {
                            *close.start()
                        } else {
                            *close.end()
                        },
                    );
                }

                if let Some(destination) = best_destination {
                    selection.collapse_to(destination, SelectionGoal::None);
                }
            })
        });
    }

    pub fn undo_selection(
        &mut self,
        _: &UndoSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.end_selection(window, cx);
        self.selection_history.mode = SelectionHistoryMode::Undoing;
        if let Some(entry) = self.selection_history.undo_stack.pop_back() {
            self.change_selections(None, window, cx, |s| {
                s.select_anchors(entry.selections.to_vec())
            });
            self.select_next_state = entry.select_next_state;
            self.select_prev_state = entry.select_prev_state;
            self.add_selections_state = entry.add_selections_state;
            self.request_autoscroll(Autoscroll::newest(), cx);
        }
        self.selection_history.mode = SelectionHistoryMode::Normal;
    }

    pub fn redo_selection(
        &mut self,
        _: &RedoSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.end_selection(window, cx);
        self.selection_history.mode = SelectionHistoryMode::Redoing;
        if let Some(entry) = self.selection_history.redo_stack.pop_back() {
            self.change_selections(None, window, cx, |s| {
                s.select_anchors(entry.selections.to_vec())
            });
            self.select_next_state = entry.select_next_state;
            self.select_prev_state = entry.select_prev_state;
            self.add_selections_state = entry.add_selections_state;
            self.request_autoscroll(Autoscroll::newest(), cx);
        }
        self.selection_history.mode = SelectionHistoryMode::Normal;
    }

    pub fn expand_excerpts(
        &mut self,
        action: &ExpandExcerpts,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.expand_excerpts_for_direction(action.lines, ExpandExcerptDirection::UpAndDown, cx)
    }

    pub fn expand_excerpts_down(
        &mut self,
        action: &ExpandExcerptsDown,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.expand_excerpts_for_direction(action.lines, ExpandExcerptDirection::Down, cx)
    }

    pub fn expand_excerpts_up(
        &mut self,
        action: &ExpandExcerptsUp,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.expand_excerpts_for_direction(action.lines, ExpandExcerptDirection::Up, cx)
    }

    pub fn expand_excerpts_for_direction(
        &mut self,
        lines: u32,
        direction: ExpandExcerptDirection,

        cx: &mut Context<Self>,
    ) {
        let selections = self.selections.disjoint_anchors();

        let lines = if lines == 0 {
            EditorSettings::get_global(cx).expand_excerpt_lines
        } else {
            lines
        };

        self.buffer.update(cx, |buffer, cx| {
            let snapshot = buffer.snapshot(cx);
            let mut excerpt_ids = selections
                .iter()
                .flat_map(|selection| snapshot.excerpt_ids_for_range(selection.range()))
                .collect::<Vec<_>>();
            excerpt_ids.sort();
            excerpt_ids.dedup();
            buffer.expand_excerpts(excerpt_ids, lines, direction, cx)
        })
    }

    pub fn expand_excerpt(
        &mut self,
        excerpt: ExcerptId,
        direction: ExpandExcerptDirection,
        cx: &mut Context<Self>,
    ) {
        let lines = EditorSettings::get_global(cx).expand_excerpt_lines;
        self.buffer.update(cx, |buffer, cx| {
            buffer.expand_excerpts([excerpt], lines, direction, cx)
        })
    }

    pub fn go_to_singleton_buffer_point(
        &mut self,
        point: Point,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.go_to_singleton_buffer_range(point..point, window, cx);
    }

    pub fn go_to_singleton_buffer_range(
        &mut self,
        range: Range<Point>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let multibuffer = self.buffer().read(cx);
        let Some(buffer) = multibuffer.as_singleton() else {
            return;
        };
        let Some(start) = multibuffer.buffer_point_to_anchor(&buffer, range.start, cx) else {
            return;
        };
        let Some(end) = multibuffer.buffer_point_to_anchor(&buffer, range.end, cx) else {
            return;
        };
        self.change_selections(Some(Autoscroll::center()), window, cx, |s| {
            s.select_anchor_ranges([start..end])
        });
    }

    fn go_to_diagnostic(
        &mut self,
        _: &GoToDiagnostic,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.go_to_diagnostic_impl(Direction::Next, window, cx)
    }

    fn go_to_prev_diagnostic(
        &mut self,
        _: &GoToPrevDiagnostic,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.go_to_diagnostic_impl(Direction::Prev, window, cx)
    }

    pub fn go_to_diagnostic_impl(
        &mut self,
        direction: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let buffer = self.buffer.read(cx).snapshot(cx);
        let selection = self.selections.newest::<usize>(cx);

        // If there is an active Diagnostic Popover jump to its diagnostic instead.
        if direction == Direction::Next {
            if let Some(popover) = self.hover_state.diagnostic_popover.as_ref() {
                let Some(buffer_id) = popover.local_diagnostic.range.start.buffer_id else {
                    return;
                };
                self.activate_diagnostics(
                    buffer_id,
                    popover.local_diagnostic.diagnostic.group_id,
                    window,
                    cx,
                );
                if let Some(active_diagnostics) = self.active_diagnostics.as_ref() {
                    let primary_range_start = active_diagnostics.primary_range.start;
                    self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                        let mut new_selection = s.newest_anchor().clone();
                        new_selection.collapse_to(primary_range_start, SelectionGoal::None);
                        s.select_anchors(vec![new_selection.clone()]);
                    });
                    self.refresh_inline_completion(false, true, window, cx);
                }
                return;
            }
        }

        let mut active_primary_range = self.active_diagnostics.as_ref().map(|active_diagnostics| {
            active_diagnostics
                .primary_range
                .to_offset(&buffer)
                .to_inclusive()
        });
        let mut search_start = if let Some(active_primary_range) = active_primary_range.as_ref() {
            if active_primary_range.contains(&selection.head()) {
                *active_primary_range.start()
            } else {
                selection.head()
            }
        } else {
            selection.head()
        };
        let snapshot = self.snapshot(window, cx);
        loop {
            let mut diagnostics;
            if direction == Direction::Prev {
                diagnostics = buffer
                    .diagnostics_in_range::<usize>(0..search_start)
                    .collect::<Vec<_>>();
                diagnostics.reverse();
            } else {
                diagnostics = buffer
                    .diagnostics_in_range::<usize>(search_start..buffer.len())
                    .collect::<Vec<_>>();
            };
            let group = diagnostics
                .into_iter()
                .filter(|diagnostic| !snapshot.intersects_fold(diagnostic.range.start))
                // relies on diagnostics_in_range to return diagnostics with the same starting range to
                // be sorted in a stable way
                // skip until we are at current active diagnostic, if it exists
                .skip_while(|entry| {
                    let is_in_range = match direction {
                        Direction::Prev => entry.range.end > search_start,
                        Direction::Next => entry.range.start < search_start,
                    };
                    is_in_range
                        && self
                            .active_diagnostics
                            .as_ref()
                            .is_some_and(|a| a.group_id != entry.diagnostic.group_id)
                })
                .find_map(|entry| {
                    if entry.diagnostic.is_primary
                        && entry.diagnostic.severity <= DiagnosticSeverity::WARNING
                        && entry.range.start != entry.range.end
                        // if we match with the active diagnostic, skip it
                        && Some(entry.diagnostic.group_id)
                            != self.active_diagnostics.as_ref().map(|d| d.group_id)
                    {
                        Some((entry.range, entry.diagnostic.group_id))
                    } else {
                        None
                    }
                });

            if let Some((primary_range, group_id)) = group {
                let Some(buffer_id) = buffer.anchor_after(primary_range.start).buffer_id else {
                    return;
                };
                self.activate_diagnostics(buffer_id, group_id, window, cx);
                if self.active_diagnostics.is_some() {
                    self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                        s.select(vec![Selection {
                            id: selection.id,
                            start: primary_range.start,
                            end: primary_range.start,
                            reversed: false,
                            goal: SelectionGoal::None,
                        }]);
                    });
                    self.refresh_inline_completion(false, true, window, cx);
                }
                break;
            } else {
                // Cycle around to the start of the buffer, potentially moving back to the start of
                // the currently active diagnostic.
                active_primary_range.take();
                if direction == Direction::Prev {
                    if search_start == buffer.len() {
                        break;
                    } else {
                        search_start = buffer.len();
                    }
                } else if search_start == 0 {
                    break;
                } else {
                    search_start = 0;
                }
            }
        }
    }

    fn go_to_next_hunk(&mut self, _: &GoToHunk, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(window, cx);
        let selection = self.selections.newest::<Point>(cx);
        self.go_to_hunk_after_position(&snapshot, selection.head(), window, cx);
    }

    fn go_to_hunk_after_position(
        &mut self,
        snapshot: &EditorSnapshot,
        position: Point,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) -> Option<MultiBufferDiffHunk> {
        let mut hunk = snapshot
            .buffer_snapshot
            .diff_hunks_in_range(position..snapshot.buffer_snapshot.max_point())
            .find(|hunk| hunk.row_range.start.0 > position.row);
        if hunk.is_none() {
            hunk = snapshot
                .buffer_snapshot
                .diff_hunks_in_range(Point::zero()..position)
                .find(|hunk| hunk.row_range.end.0 < position.row)
        }
        if let Some(hunk) = &hunk {
            let destination = Point::new(hunk.row_range.start.0, 0);
            self.unfold_ranges(&[destination..destination], false, false, cx);
            self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select_ranges(vec![destination..destination]);
            });
        }

        hunk
    }

    fn go_to_prev_hunk(&mut self, _: &GoToPrevHunk, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(window, cx);
        let selection = self.selections.newest::<Point>(cx);
        self.go_to_hunk_before_position(&snapshot, selection.head(), window, cx);
    }

    fn go_to_hunk_before_position(
        &mut self,
        snapshot: &EditorSnapshot,
        position: Point,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) -> Option<MultiBufferDiffHunk> {
        let mut hunk = snapshot.buffer_snapshot.diff_hunk_before(position);
        if hunk.is_none() {
            hunk = snapshot.buffer_snapshot.diff_hunk_before(Point::MAX);
        }
        if let Some(hunk) = &hunk {
            let destination = Point::new(hunk.row_range.start.0, 0);
            self.unfold_ranges(&[destination..destination], false, false, cx);
            self.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                s.select_ranges(vec![destination..destination]);
            });
        }

        hunk
    }

    pub fn go_to_definition(
        &mut self,
        _: &GoToDefinition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Navigated>> {
        let definition =
            self.go_to_definition_of_kind(GotoDefinitionKind::Symbol, false, window, cx);
        cx.spawn_in(window, |editor, mut cx| async move {
            if definition.await? == Navigated::Yes {
                return Ok(Navigated::Yes);
            }
            match editor.update_in(&mut cx, |editor, window, cx| {
                editor.find_all_references(&FindAllReferences, window, cx)
            })? {
                Some(references) => references.await,
                None => Ok(Navigated::No),
            }
        })
    }

    pub fn go_to_declaration(
        &mut self,
        _: &GoToDeclaration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Navigated>> {
        self.go_to_definition_of_kind(GotoDefinitionKind::Declaration, false, window, cx)
    }

    pub fn go_to_declaration_split(
        &mut self,
        _: &GoToDeclaration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Navigated>> {
        self.go_to_definition_of_kind(GotoDefinitionKind::Declaration, true, window, cx)
    }

    pub fn go_to_implementation(
        &mut self,
        _: &GoToImplementation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Navigated>> {
        self.go_to_definition_of_kind(GotoDefinitionKind::Implementation, false, window, cx)
    }

    pub fn go_to_implementation_split(
        &mut self,
        _: &GoToImplementationSplit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Navigated>> {
        self.go_to_definition_of_kind(GotoDefinitionKind::Implementation, true, window, cx)
    }

    pub fn go_to_type_definition(
        &mut self,
        _: &GoToTypeDefinition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Navigated>> {
        self.go_to_definition_of_kind(GotoDefinitionKind::Type, false, window, cx)
    }

    pub fn go_to_definition_split(
        &mut self,
        _: &GoToDefinitionSplit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Navigated>> {
        self.go_to_definition_of_kind(GotoDefinitionKind::Symbol, true, window, cx)
    }

    pub fn go_to_type_definition_split(
        &mut self,
        _: &GoToTypeDefinitionSplit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Navigated>> {
        self.go_to_definition_of_kind(GotoDefinitionKind::Type, true, window, cx)
    }

    fn go_to_definition_of_kind(
        &mut self,
        kind: GotoDefinitionKind,
        split: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Navigated>> {
        let Some(provider) = self.semantics_provider.clone() else {
            return Task::ready(Ok(Navigated::No));
        };
        let head = self.selections.newest::<usize>(cx).head();
        let buffer = self.buffer.read(cx);
        let (buffer, head) = if let Some(text_anchor) = buffer.text_anchor_for_position(head, cx) {
            text_anchor
        } else {
            return Task::ready(Ok(Navigated::No));
        };

        let Some(definitions) = provider.definitions(&buffer, head, kind, cx) else {
            return Task::ready(Ok(Navigated::No));
        };

        cx.spawn_in(window, |editor, mut cx| async move {
            let definitions = definitions.await?;
            let navigated = editor
                .update_in(&mut cx, |editor, window, cx| {
                    editor.navigate_to_hover_links(
                        Some(kind),
                        definitions
                            .into_iter()
                            .filter(|location| {
                                hover_links::exclude_link_to_position(&buffer, &head, location, cx)
                            })
                            .map(HoverLink::Text)
                            .collect::<Vec<_>>(),
                        split,
                        window,
                        cx,
                    )
                })?
                .await?;
            anyhow::Ok(navigated)
        })
    }

    pub fn open_url(&mut self, _: &OpenUrl, window: &mut Window, cx: &mut Context<Self>) {
        let selection = self.selections.newest_anchor();
        let head = selection.head();
        let tail = selection.tail();

        let Some((buffer, start_position)) =
            self.buffer.read(cx).text_anchor_for_position(head, cx)
        else {
            return;
        };

        let end_position = if head != tail {
            let Some((_, pos)) = self.buffer.read(cx).text_anchor_for_position(tail, cx) else {
                return;
            };
            Some(pos)
        } else {
            None
        };

        let url_finder = cx.spawn_in(window, |editor, mut cx| async move {
            let url = if let Some(end_pos) = end_position {
                find_url_from_range(&buffer, start_position..end_pos, cx.clone())
            } else {
                find_url(&buffer, start_position, cx.clone()).map(|(_, url)| url)
            };

            if let Some(url) = url {
                editor.update(&mut cx, |_, cx| {
                    cx.open_url(&url);
                })
            } else {
                Ok(())
            }
        });

        url_finder.detach();
    }

    pub fn open_selected_filename(
        &mut self,
        _: &OpenSelectedFilename,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace() else {
            return;
        };

        let position = self.selections.newest_anchor().head();

        let Some((buffer, buffer_position)) =
            self.buffer.read(cx).text_anchor_for_position(position, cx)
        else {
            return;
        };

        let project = self.project.clone();

        cx.spawn_in(window, |_, mut cx| async move {
            let result = find_file(&buffer, project, buffer_position, &mut cx).await;

            if let Some((_, path)) = result {
                workspace
                    .update_in(&mut cx, |workspace, window, cx| {
                        workspace.open_resolved_path(path, window, cx)
                    })?
                    .await?;
            }
            anyhow::Ok(())
        })
        .detach();
    }

    pub(crate) fn navigate_to_hover_links(
        &mut self,
        kind: Option<GotoDefinitionKind>,
        mut definitions: Vec<HoverLink>,
        split: bool,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) -> Task<Result<Navigated>> {
        // If there is one definition, just open it directly
        if definitions.len() == 1 {
            let definition = definitions.pop().unwrap();

            enum TargetTaskResult {
                Location(Option<Location>),
                AlreadyNavigated,
            }

            let target_task = match definition {
                HoverLink::Text(link) => {
                    Task::ready(anyhow::Ok(TargetTaskResult::Location(Some(link.target))))
                }
                HoverLink::InlayHint(lsp_location, server_id) => {
                    let computation =
                        self.compute_target_location(lsp_location, server_id, window, cx);
                    cx.background_executor().spawn(async move {
                        let location = computation.await?;
                        Ok(TargetTaskResult::Location(location))
                    })
                }
                HoverLink::Url(url) => {
                    cx.open_url(&url);
                    Task::ready(Ok(TargetTaskResult::AlreadyNavigated))
                }
                HoverLink::File(path) => {
                    if let Some(workspace) = self.workspace() {
                        cx.spawn_in(window, |_, mut cx| async move {
                            workspace
                                .update_in(&mut cx, |workspace, window, cx| {
                                    workspace.open_resolved_path(path, window, cx)
                                })?
                                .await
                                .map(|_| TargetTaskResult::AlreadyNavigated)
                        })
                    } else {
                        Task::ready(Ok(TargetTaskResult::Location(None)))
                    }
                }
            };
            cx.spawn_in(window, |editor, mut cx| async move {
                let target = match target_task.await.context("target resolution task")? {
                    TargetTaskResult::AlreadyNavigated => return Ok(Navigated::Yes),
                    TargetTaskResult::Location(None) => return Ok(Navigated::No),
                    TargetTaskResult::Location(Some(target)) => target,
                };

                editor.update_in(&mut cx, |editor, window, cx| {
                    let Some(workspace) = editor.workspace() else {
                        return Navigated::No;
                    };
                    let pane = workspace.read(cx).active_pane().clone();

                    let range = target.range.to_point(target.buffer.read(cx));
                    let range = editor.range_for_match(&range);
                    let range = collapse_multiline_range(range);

                    if Some(&target.buffer) == editor.buffer.read(cx).as_singleton().as_ref() {
                        editor.go_to_singleton_buffer_range(range.clone(), window, cx);
                    } else {
                        window.defer(cx, move |window, cx| {
                            let target_editor: Entity<Self> =
                                workspace.update(cx, |workspace, cx| {
                                    let pane = if split {
                                        workspace.adjacent_pane(window, cx)
                                    } else {
                                        workspace.active_pane().clone()
                                    };

                                    workspace.open_project_item(
                                        pane,
                                        target.buffer.clone(),
                                        true,
                                        true,
                                        window,
                                        cx,
                                    )
                                });
                            target_editor.update(cx, |target_editor, cx| {
                                // When selecting a definition in a different buffer, disable the nav history
                                // to avoid creating a history entry at the previous cursor location.
                                pane.update(cx, |pane, _| pane.disable_history());
                                target_editor.go_to_singleton_buffer_range(range, window, cx);
                                pane.update(cx, |pane, _| pane.enable_history());
                            });
                        });
                    }
                    Navigated::Yes
                })
            })
        } else if !definitions.is_empty() {
            cx.spawn_in(window, |editor, mut cx| async move {
                let (title, location_tasks, workspace) = editor
                    .update_in(&mut cx, |editor, window, cx| {
                        let tab_kind = match kind {
                            Some(GotoDefinitionKind::Implementation) => "Implementations",
                            _ => "Definitions",
                        };
                        let title = definitions
                            .iter()
                            .find_map(|definition| match definition {
                                HoverLink::Text(link) => link.origin.as_ref().map(|origin| {
                                    let buffer = origin.buffer.read(cx);
                                    format!(
                                        "{} for {}",
                                        tab_kind,
                                        buffer
                                            .text_for_range(origin.range.clone())
                                            .collect::<String>()
                                    )
                                }),
                                HoverLink::InlayHint(_, _) => None,
                                HoverLink::Url(_) => None,
                                HoverLink::File(_) => None,
                            })
                            .unwrap_or(tab_kind.to_string());
                        let location_tasks = definitions
                            .into_iter()
                            .map(|definition| match definition {
                                HoverLink::Text(link) => Task::ready(Ok(Some(link.target))),
                                HoverLink::InlayHint(lsp_location, server_id) => editor
                                    .compute_target_location(lsp_location, server_id, window, cx),
                                HoverLink::Url(_) => Task::ready(Ok(None)),
                                HoverLink::File(_) => Task::ready(Ok(None)),
                            })
                            .collect::<Vec<_>>();
                        (title, location_tasks, editor.workspace().clone())
                    })
                    .context("location tasks preparation")?;

                let locations = future::join_all(location_tasks)
                    .await
                    .into_iter()
                    .filter_map(|location| location.transpose())
                    .collect::<Result<_>>()
                    .context("location tasks")?;

                let Some(workspace) = workspace else {
                    return Ok(Navigated::No);
                };
                let opened = workspace
                    .update_in(&mut cx, |workspace, window, cx| {
                        Self::open_locations_in_multibuffer(
                            workspace,
                            locations,
                            title,
                            split,
                            MultibufferSelectionMode::First,
                            window,
                            cx,
                        )
                    })
                    .ok();

                anyhow::Ok(Navigated::from_bool(opened.is_some()))
            })
        } else {
            Task::ready(Ok(Navigated::No))
        }
    }

    fn compute_target_location(
        &self,
        lsp_location: lsp::Location,
        server_id: LanguageServerId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Option<Location>>> {
        let Some(project) = self.project.clone() else {
            return Task::ready(Ok(None));
        };

        cx.spawn_in(window, move |editor, mut cx| async move {
            let location_task = editor.update(&mut cx, |_, cx| {
                project.update(cx, |project, cx| {
                    let language_server_name = project
                        .language_server_statuses(cx)
                        .find(|(id, _)| server_id == *id)
                        .map(|(_, status)| LanguageServerName::from(status.name.as_str()));
                    language_server_name.map(|language_server_name| {
                        project.open_local_buffer_via_lsp(
                            lsp_location.uri.clone(),
                            server_id,
                            language_server_name,
                            cx,
                        )
                    })
                })
            })?;
            let location = match location_task {
                Some(task) => Some({
                    let target_buffer_handle = task.await.context("open local buffer")?;
                    let range = target_buffer_handle.update(&mut cx, |target_buffer, _| {
                        let target_start = target_buffer
                            .clip_point_utf16(point_from_lsp(lsp_location.range.start), Bias::Left);
                        let target_end = target_buffer
                            .clip_point_utf16(point_from_lsp(lsp_location.range.end), Bias::Left);
                        target_buffer.anchor_after(target_start)
                            ..target_buffer.anchor_before(target_end)
                    })?;
                    Location {
                        buffer: target_buffer_handle,
                        range,
                    }
                }),
                None => None,
            };
            Ok(location)
        })
    }

    pub fn find_all_references(
        &mut self,
        _: &FindAllReferences,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<Navigated>>> {
        let selection = self.selections.newest::<usize>(cx);
        let multi_buffer = self.buffer.read(cx);
        let head = selection.head();

        let multi_buffer_snapshot = multi_buffer.snapshot(cx);
        let head_anchor = multi_buffer_snapshot.anchor_at(
            head,
            if head < selection.tail() {
                Bias::Right
            } else {
                Bias::Left
            },
        );

        match self
            .find_all_references_task_sources
            .binary_search_by(|anchor| anchor.cmp(&head_anchor, &multi_buffer_snapshot))
        {
            Ok(_) => {
                log::info!(
                    "Ignoring repeated FindAllReferences invocation with the position of already running task"
                );
                return None;
            }
            Err(i) => {
                self.find_all_references_task_sources.insert(i, head_anchor);
            }
        }

        let (buffer, head) = multi_buffer.text_anchor_for_position(head, cx)?;
        let workspace = self.workspace()?;
        let project = workspace.read(cx).project().clone();
        let references = project.update(cx, |project, cx| project.references(&buffer, head, cx));
        Some(cx.spawn_in(window, |editor, mut cx| async move {
            let _cleanup = defer({
                let mut cx = cx.clone();
                move || {
                    let _ = editor.update(&mut cx, |editor, _| {
                        if let Ok(i) =
                            editor
                                .find_all_references_task_sources
                                .binary_search_by(|anchor| {
                                    anchor.cmp(&head_anchor, &multi_buffer_snapshot)
                                })
                        {
                            editor.find_all_references_task_sources.remove(i);
                        }
                    });
                }
            });

            let locations = references.await?;
            if locations.is_empty() {
                return anyhow::Ok(Navigated::No);
            }

            workspace.update_in(&mut cx, |workspace, window, cx| {
                let title = locations
                    .first()
                    .as_ref()
                    .map(|location| {
                        let buffer = location.buffer.read(cx);
                        format!(
                            "References to `{}`",
                            buffer
                                .text_for_range(location.range.clone())
                                .collect::<String>()
                        )
                    })
                    .unwrap();
                Self::open_locations_in_multibuffer(
                    workspace,
                    locations,
                    title,
                    false,
                    MultibufferSelectionMode::First,
                    window,
                    cx,
                );
                Navigated::Yes
            })
        }))
    }

    /// Opens a multibuffer with the given project locations in it
    pub fn open_locations_in_multibuffer(
        workspace: &mut Workspace,
        mut locations: Vec<Location>,
        title: String,
        split: bool,
        multibuffer_selection_mode: MultibufferSelectionMode,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        // If there are multiple definitions, open them in a multibuffer
        locations.sort_by_key(|location| location.buffer.read(cx).remote_id());
        let mut locations = locations.into_iter().peekable();
        let mut ranges = Vec::new();
        let capability = workspace.project().read(cx).capability();

        let excerpt_buffer = cx.new(|cx| {
            let mut multibuffer = MultiBuffer::new(capability);
            while let Some(location) = locations.next() {
                let buffer = location.buffer.read(cx);
                let mut ranges_for_buffer = Vec::new();
                let range = location.range.to_offset(buffer);
                ranges_for_buffer.push(range.clone());

                while let Some(next_location) = locations.peek() {
                    if next_location.buffer == location.buffer {
                        ranges_for_buffer.push(next_location.range.to_offset(buffer));
                        locations.next();
                    } else {
                        break;
                    }
                }

                ranges_for_buffer.sort_by_key(|range| (range.start, Reverse(range.end)));
                ranges.extend(multibuffer.push_excerpts_with_context_lines(
                    location.buffer.clone(),
                    ranges_for_buffer,
                    DEFAULT_MULTIBUFFER_CONTEXT,
                    cx,
                ))
            }

            multibuffer.with_title(title)
        });

        let editor = cx.new(|cx| {
            Editor::for_multibuffer(
                excerpt_buffer,
                Some(workspace.project().clone()),
                true,
                window,
                cx,
            )
        });
        editor.update(cx, |editor, cx| {
            match multibuffer_selection_mode {
                MultibufferSelectionMode::First => {
                    if let Some(first_range) = ranges.first() {
                        editor.change_selections(None, window, cx, |selections| {
                            selections.clear_disjoint();
                            selections.select_anchor_ranges(std::iter::once(first_range.clone()));
                        });
                    }
                    editor.highlight_background::<Self>(
                        &ranges,
                        |theme| theme.editor_highlighted_line_background,
                        cx,
                    );
                }
                MultibufferSelectionMode::All => {
                    editor.change_selections(None, window, cx, |selections| {
                        selections.clear_disjoint();
                        selections.select_anchor_ranges(ranges);
                    });
                }
            }
            editor.register_buffers_with_language_servers(cx);
        });

        let item = Box::new(editor);
        let item_id = item.item_id();

        if split {
            workspace.split_item(SplitDirection::Right, item.clone(), window, cx);
        } else {
            let destination_index = workspace.active_pane().update(cx, |pane, cx| {
                if PreviewTabsSettings::get_global(cx).enable_preview_from_code_navigation {
                    pane.close_current_preview_item(window, cx)
                } else {
                    None
                }
            });
            workspace.add_item_to_active_pane(item.clone(), destination_index, true, window, cx);
        }
        workspace.active_pane().update(cx, |pane, cx| {
            pane.set_preview_item_id(Some(item_id), cx);
        });
    }

    pub fn rename(
        &mut self,
        _: &Rename,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        use language::ToOffset as _;

        let provider = self.semantics_provider.clone()?;
        let selection = self.selections.newest_anchor().clone();
        let (cursor_buffer, cursor_buffer_position) = self
            .buffer
            .read(cx)
            .text_anchor_for_position(selection.head(), cx)?;
        let (tail_buffer, cursor_buffer_position_end) = self
            .buffer
            .read(cx)
            .text_anchor_for_position(selection.tail(), cx)?;
        if tail_buffer != cursor_buffer {
            return None;
        }

        let snapshot = cursor_buffer.read(cx).snapshot();
        let cursor_buffer_offset = cursor_buffer_position.to_offset(&snapshot);
        let cursor_buffer_offset_end = cursor_buffer_position_end.to_offset(&snapshot);
        let prepare_rename = provider
            .range_for_rename(&cursor_buffer, cursor_buffer_position, cx)
            .unwrap_or_else(|| Task::ready(Ok(None)));
        drop(snapshot);

        Some(cx.spawn_in(window, |this, mut cx| async move {
            let rename_range = if let Some(range) = prepare_rename.await? {
                Some(range)
            } else {
                this.update(&mut cx, |this, cx| {
                    let buffer = this.buffer.read(cx).snapshot(cx);
                    let mut buffer_highlights = this
                        .document_highlights_for_position(selection.head(), &buffer)
                        .filter(|highlight| {
                            highlight.start.excerpt_id == selection.head().excerpt_id
                                && highlight.end.excerpt_id == selection.head().excerpt_id
                        });
                    buffer_highlights
                        .next()
                        .map(|highlight| highlight.start.text_anchor..highlight.end.text_anchor)
                })?
            };
            if let Some(rename_range) = rename_range {
                this.update_in(&mut cx, |this, window, cx| {
                    let snapshot = cursor_buffer.read(cx).snapshot();
                    let rename_buffer_range = rename_range.to_offset(&snapshot);
                    let cursor_offset_in_rename_range =
                        cursor_buffer_offset.saturating_sub(rename_buffer_range.start);
                    let cursor_offset_in_rename_range_end =
                        cursor_buffer_offset_end.saturating_sub(rename_buffer_range.start);

                    this.take_rename(false, window, cx);
                    let buffer = this.buffer.read(cx).read(cx);
                    let cursor_offset = selection.head().to_offset(&buffer);
                    let rename_start = cursor_offset.saturating_sub(cursor_offset_in_rename_range);
                    let rename_end = rename_start + rename_buffer_range.len();
                    let range = buffer.anchor_before(rename_start)..buffer.anchor_after(rename_end);
                    let mut old_highlight_id = None;
                    let old_name: Arc<str> = buffer
                        .chunks(rename_start..rename_end, true)
                        .map(|chunk| {
                            if old_highlight_id.is_none() {
                                old_highlight_id = chunk.syntax_highlight_id;
                            }
                            chunk.text
                        })
                        .collect::<String>()
                        .into();

                    drop(buffer);

                    // Position the selection in the rename editor so that it matches the current selection.
                    this.show_local_selections = false;
                    let rename_editor = cx.new(|cx| {
                        let mut editor = Editor::single_line(window, cx);
                        editor.buffer.update(cx, |buffer, cx| {
                            buffer.edit([(0..0, old_name.clone())], None, cx)
                        });
                        let rename_selection_range = match cursor_offset_in_rename_range
                            .cmp(&cursor_offset_in_rename_range_end)
                        {
                            Ordering::Equal => {
                                editor.select_all(&SelectAll, window, cx);
                                return editor;
                            }
                            Ordering::Less => {
                                cursor_offset_in_rename_range..cursor_offset_in_rename_range_end
                            }
                            Ordering::Greater => {
                                cursor_offset_in_rename_range_end..cursor_offset_in_rename_range
                            }
                        };
                        if rename_selection_range.end > old_name.len() {
                            editor.select_all(&SelectAll, window, cx);
                        } else {
                            editor.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                                s.select_ranges([rename_selection_range]);
                            });
                        }
                        editor
                    });
                    cx.subscribe(&rename_editor, |_, _, e: &EditorEvent, cx| {
                        if e == &EditorEvent::Focused {
                            cx.emit(EditorEvent::FocusedIn)
                        }
                    })
                    .detach();

                    let write_highlights =
                        this.clear_background_highlights::<DocumentHighlightWrite>(cx);
                    let read_highlights =
                        this.clear_background_highlights::<DocumentHighlightRead>(cx);
                    let ranges = write_highlights
                        .iter()
                        .flat_map(|(_, ranges)| ranges.iter())
                        .chain(read_highlights.iter().flat_map(|(_, ranges)| ranges.iter()))
                        .cloned()
                        .collect();

                    this.highlight_text::<Rename>(
                        ranges,
                        HighlightStyle {
                            fade_out: Some(0.6),
                            ..Default::default()
                        },
                        cx,
                    );
                    let rename_focus_handle = rename_editor.focus_handle(cx);
                    window.focus(&rename_focus_handle);
                    let block_id = this.insert_blocks(
                        [BlockProperties {
                            style: BlockStyle::Flex,
                            placement: BlockPlacement::Below(range.start),
                            height: 1,
                            render: Arc::new({
                                let rename_editor = rename_editor.clone();
                                move |cx: &mut BlockContext| {
                                    let mut text_style = cx.editor_style.text.clone();
                                    if let Some(highlight_style) = old_highlight_id
                                        .and_then(|h| h.style(&cx.editor_style.syntax))
                                    {
                                        text_style = text_style.highlight(highlight_style);
                                    }
                                    div()
                                        .block_mouse_down()
                                        .pl(cx.anchor_x)
                                        .child(EditorElement::new(
                                            &rename_editor,
                                            EditorStyle {
                                                background: cx.theme().system().transparent,
                                                local_player: cx.editor_style.local_player,
                                                text: text_style,
                                                scrollbar_width: cx.editor_style.scrollbar_width,
                                                syntax: cx.editor_style.syntax.clone(),
                                                status: cx.editor_style.status.clone(),
                                                inlay_hints_style: HighlightStyle {
                                                    font_weight: Some(FontWeight::BOLD),
                                                    ..make_inlay_hints_style(cx.app)
                                                },
                                                inline_completion_styles: make_suggestion_styles(
                                                    cx.app,
                                                ),
                                                ..EditorStyle::default()
                                            },
                                        ))
                                        .into_any_element()
                                }
                            }),
                            priority: 0,
                        }],
                        Some(Autoscroll::fit()),
                        cx,
                    )[0];
                    this.pending_rename = Some(RenameState {
                        range,
                        old_name,
                        editor: rename_editor,
                        block_id,
                    });
                })?;
            }

            Ok(())
        }))
    }

    pub fn confirm_rename(
        &mut self,
        _: &ConfirmRename,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        let rename = self.take_rename(false, window, cx)?;
        let workspace = self.workspace()?.downgrade();
        let (buffer, start) = self
            .buffer
            .read(cx)
            .text_anchor_for_position(rename.range.start, cx)?;
        let (end_buffer, _) = self
            .buffer
            .read(cx)
            .text_anchor_for_position(rename.range.end, cx)?;
        if buffer != end_buffer {
            return None;
        }

        let old_name = rename.old_name;
        let new_name = rename.editor.read(cx).text(cx);

        let rename = self.semantics_provider.as_ref()?.perform_rename(
            &buffer,
            start,
            new_name.clone(),
            cx,
        )?;

        Some(cx.spawn_in(window, |editor, mut cx| async move {
            let project_transaction = rename.await?;
            Self::open_project_transaction(
                &editor,
                workspace,
                project_transaction,
                format!("Rename: {} → {}", old_name, new_name),
                cx.clone(),
            )
            .await?;

            editor.update(&mut cx, |editor, cx| {
                editor.refresh_document_highlights(cx);
            })?;
            Ok(())
        }))
    }

    fn take_rename(
        &mut self,
        moving_cursor: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<RenameState> {
        let rename = self.pending_rename.take()?;
        if rename.editor.focus_handle(cx).is_focused(window) {
            window.focus(&self.focus_handle);
        }

        self.remove_blocks(
            [rename.block_id].into_iter().collect(),
            Some(Autoscroll::fit()),
            cx,
        );
        self.clear_highlights::<Rename>(cx);
        self.show_local_selections = true;

        if moving_cursor {
            let cursor_in_rename_editor = rename.editor.update(cx, |editor, cx| {
                editor.selections.newest::<usize>(cx).head()
            });

            // Update the selection to match the position of the selection inside
            // the rename editor.
            let snapshot = self.buffer.read(cx).read(cx);
            let rename_range = rename.range.to_offset(&snapshot);
            let cursor_in_editor = snapshot
                .clip_offset(rename_range.start + cursor_in_rename_editor, Bias::Left)
                .min(rename_range.end);
            drop(snapshot);

            self.change_selections(None, window, cx, |s| {
                s.select_ranges(vec![cursor_in_editor..cursor_in_editor])
            });
        } else {
            self.refresh_document_highlights(cx);
        }

        Some(rename)
    }

    pub fn pending_rename(&self) -> Option<&RenameState> {
        self.pending_rename.as_ref()
    }

    fn format(
        &mut self,
        _: &Format,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        let project = match &self.project {
            Some(project) => project.clone(),
            None => return None,
        };

        Some(self.perform_format(
            project,
            FormatTrigger::Manual,
            FormatTarget::Buffers,
            window,
            cx,
        ))
    }

    fn format_selections(
        &mut self,
        _: &FormatSelections,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        let project = match &self.project {
            Some(project) => project.clone(),
            None => return None,
        };

        let ranges = self
            .selections
            .all_adjusted(cx)
            .into_iter()
            .map(|selection| selection.range())
            .collect_vec();

        Some(self.perform_format(
            project,
            FormatTrigger::Manual,
            FormatTarget::Ranges(ranges),
            window,
            cx,
        ))
    }

    fn perform_format(
        &mut self,
        project: Entity<Project>,
        trigger: FormatTrigger,
        target: FormatTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let buffer = self.buffer.clone();
        let (buffers, target) = match target {
            FormatTarget::Buffers => {
                let mut buffers = buffer.read(cx).all_buffers();
                if trigger == FormatTrigger::Save {
                    buffers.retain(|buffer| buffer.read(cx).is_dirty());
                }
                (buffers, LspFormatTarget::Buffers)
            }
            FormatTarget::Ranges(selection_ranges) => {
                let multi_buffer = buffer.read(cx);
                let snapshot = multi_buffer.read(cx);
                let mut buffers = HashSet::default();
                let mut buffer_id_to_ranges: BTreeMap<BufferId, Vec<Range<text::Anchor>>> =
                    BTreeMap::new();
                for selection_range in selection_ranges {
                    for (buffer, buffer_range, _) in
                        snapshot.range_to_buffer_ranges(selection_range)
                    {
                        let buffer_id = buffer.remote_id();
                        let start = buffer.anchor_before(buffer_range.start);
                        let end = buffer.anchor_after(buffer_range.end);
                        buffers.insert(multi_buffer.buffer(buffer_id).unwrap());
                        buffer_id_to_ranges
                            .entry(buffer_id)
                            .and_modify(|buffer_ranges| buffer_ranges.push(start..end))
                            .or_insert_with(|| vec![start..end]);
                    }
                }
                (buffers, LspFormatTarget::Ranges(buffer_id_to_ranges))
            }
        };

        let mut timeout = cx.background_executor().timer(FORMAT_TIMEOUT).fuse();
        let format = project.update(cx, |project, cx| {
            project.format(buffers, target, true, trigger, cx)
        });

        cx.spawn_in(window, |_, mut cx| async move {
            let transaction = futures::select_biased! {
                () = timeout => {
                    log::warn!("timed out waiting for formatting");
                    None
                }
                transaction = format.log_err().fuse() => transaction,
            };

            buffer
                .update(&mut cx, |buffer, cx| {
                    if let Some(transaction) = transaction {
                        if !buffer.is_singleton() {
                            buffer.push_transaction(&transaction.0, cx);
                        }
                    }

                    cx.notify();
                })
                .ok();

            Ok(())
        })
    }

    fn restart_language_server(
        &mut self,
        _: &RestartLanguageServer,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(project) = self.project.clone() {
            self.buffer.update(cx, |multi_buffer, cx| {
                project.update(cx, |project, cx| {
                    project.restart_language_servers_for_buffers(multi_buffer.all_buffers(), cx);
                });
            })
        }
    }

    fn cancel_language_server_work(
        workspace: &mut Workspace,
        _: &actions::CancelLanguageServerWork,
        _: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let project = workspace.project();
        let buffers = workspace
            .active_item(cx)
            .and_then(|item| item.act_as::<Editor>(cx))
            .map_or(HashSet::default(), |editor| {
                editor.read(cx).buffer.read(cx).all_buffers()
            });
        project.update(cx, |project, cx| {
            project.cancel_language_server_work_for_buffers(buffers, cx);
        });
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn refresh_active_diagnostics(&mut self, cx: &mut Context<Editor>) {
        if let Some(active_diagnostics) = self.active_diagnostics.as_mut() {
            let buffer = self.buffer.read(cx).snapshot(cx);
            let primary_range_start = active_diagnostics.primary_range.start.to_offset(&buffer);
            let primary_range_end = active_diagnostics.primary_range.end.to_offset(&buffer);
            let is_valid = buffer
                .diagnostics_in_range::<usize>(primary_range_start..primary_range_end)
                .any(|entry| {
                    entry.diagnostic.is_primary
                        && !entry.range.is_empty()
                        && entry.range.start == primary_range_start
                        && entry.diagnostic.message == active_diagnostics.primary_message
                });

            if is_valid != active_diagnostics.is_valid {
                active_diagnostics.is_valid = is_valid;
                let mut new_styles = HashMap::default();
                for (block_id, diagnostic) in &active_diagnostics.blocks {
                    new_styles.insert(
                        *block_id,
                        diagnostic_block_renderer(diagnostic.clone(), None, true, is_valid),
                    );
                }
                self.display_map.update(cx, |display_map, _cx| {
                    display_map.replace_blocks(new_styles)
                });
            }
        }
    }

    fn activate_diagnostics(
        &mut self,
        buffer_id: BufferId,
        group_id: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_diagnostics(cx);
        let snapshot = self.snapshot(window, cx);
        self.active_diagnostics = self.display_map.update(cx, |display_map, cx| {
            let buffer = self.buffer.read(cx).snapshot(cx);

            let mut primary_range = None;
            let mut primary_message = None;
            let diagnostic_group = buffer
                .diagnostic_group(buffer_id, group_id)
                .filter_map(|entry| {
                    let start = entry.range.start;
                    let end = entry.range.end;
                    if snapshot.is_line_folded(MultiBufferRow(start.row))
                        && (start.row == end.row
                            || snapshot.is_line_folded(MultiBufferRow(end.row)))
                    {
                        return None;
                    }
                    if entry.diagnostic.is_primary {
                        primary_range = Some(entry.range.clone());
                        primary_message = Some(entry.diagnostic.message.clone());
                    }
                    Some(entry)
                })
                .collect::<Vec<_>>();
            let primary_range = primary_range?;
            let primary_message = primary_message?;

            let blocks = display_map
                .insert_blocks(
                    diagnostic_group.iter().map(|entry| {
                        let diagnostic = entry.diagnostic.clone();
                        let message_height = diagnostic.message.matches('\n').count() as u32 + 1;
                        BlockProperties {
                            style: BlockStyle::Fixed,
                            placement: BlockPlacement::Below(
                                buffer.anchor_after(entry.range.start),
                            ),
                            height: message_height,
                            render: diagnostic_block_renderer(diagnostic, None, true, true),
                            priority: 0,
                        }
                    }),
                    cx,
                )
                .into_iter()
                .zip(diagnostic_group.into_iter().map(|entry| entry.diagnostic))
                .collect();

            Some(ActiveDiagnosticGroup {
                primary_range: buffer.anchor_before(primary_range.start)
                    ..buffer.anchor_after(primary_range.end),
                primary_message,
                group_id,
                blocks,
                is_valid: true,
            })
        });
    }

    fn dismiss_diagnostics(&mut self, cx: &mut Context<Self>) {
        if let Some(active_diagnostic_group) = self.active_diagnostics.take() {
            self.display_map.update(cx, |display_map, cx| {
                display_map.remove_blocks(active_diagnostic_group.blocks.into_keys().collect(), cx);
            });
            cx.notify();
        }
    }

    pub fn set_selections_from_remote(
        &mut self,
        selections: Vec<Selection<Anchor>>,
        pending_selection: Option<Selection<Anchor>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let old_cursor_position = self.selections.newest_anchor().head();
        self.selections.change_with(cx, |s| {
            s.select_anchors(selections);
            if let Some(pending_selection) = pending_selection {
                s.set_pending(pending_selection, SelectMode::Character);
            } else {
                s.clear_pending();
            }
        });
        self.selections_did_change(false, &old_cursor_position, true, window, cx);
    }

    fn push_to_selection_history(&mut self) {
        self.selection_history.push(SelectionHistoryEntry {
            selections: self.selections.disjoint_anchors(),
            select_next_state: self.select_next_state.clone(),
            select_prev_state: self.select_prev_state.clone(),
            add_selections_state: self.add_selections_state.clone(),
        });
    }

    pub fn transact(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>),
    ) -> Option<TransactionId> {
        self.start_transaction_at(Instant::now(), window, cx);
        update(self, window, cx);
        self.end_transaction_at(Instant::now(), cx)
    }

    pub fn start_transaction_at(
        &mut self,
        now: Instant,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.end_selection(window, cx);
        if let Some(tx_id) = self
            .buffer
            .update(cx, |buffer, cx| buffer.start_transaction_at(now, cx))
        {
            self.selection_history
                .insert_transaction(tx_id, self.selections.disjoint_anchors());
            cx.emit(EditorEvent::TransactionBegun {
                transaction_id: tx_id,
            })
        }
    }

    pub fn end_transaction_at(
        &mut self,
        now: Instant,
        cx: &mut Context<Self>,
    ) -> Option<TransactionId> {
        if let Some(transaction_id) = self
            .buffer
            .update(cx, |buffer, cx| buffer.end_transaction_at(now, cx))
        {
            if let Some((_, end_selections)) =
                self.selection_history.transaction_mut(transaction_id)
            {
                *end_selections = Some(self.selections.disjoint_anchors());
            } else {
                log::error!("unexpectedly ended a transaction that wasn't started by this editor");
            }

            cx.emit(EditorEvent::Edited { transaction_id });
            Some(transaction_id)
        } else {
            None
        }
    }

    pub fn set_mark(&mut self, _: &actions::SetMark, window: &mut Window, cx: &mut Context<Self>) {
        if self.selection_mark_mode {
            self.change_selections(None, window, cx, |s| {
                s.move_with(|_, sel| {
                    sel.collapse_to(sel.head(), SelectionGoal::None);
                });
            })
        }
        self.selection_mark_mode = true;
        cx.notify();
    }

    pub fn swap_selection_ends(
        &mut self,
        _: &actions::SwapSelectionEnds,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(None, window, cx, |s| {
            s.move_with(|_, sel| {
                if sel.start != sel.end {
                    sel.reversed = !sel.reversed
                }
            });
        });
        self.request_autoscroll(Autoscroll::newest(), cx);
        cx.notify();
    }

    pub fn toggle_fold(
        &mut self,
        _: &actions::ToggleFold,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_singleton(cx) {
            let selection = self.selections.newest::<Point>(cx);

            let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
            let range = if selection.is_empty() {
                let point = selection.head().to_display_point(&display_map);
                let start = DisplayPoint::new(point.row(), 0).to_point(&display_map);
                let end = DisplayPoint::new(point.row(), display_map.line_len(point.row()))
                    .to_point(&display_map);
                start..end
            } else {
                selection.range()
            };
            if display_map.folds_in_range(range).next().is_some() {
                self.unfold_lines(&Default::default(), window, cx)
            } else {
                self.fold(&Default::default(), window, cx)
            }
        } else {
            let multi_buffer_snapshot = self.buffer.read(cx).snapshot(cx);
            let buffer_ids: HashSet<_> = multi_buffer_snapshot
                .ranges_to_buffer_ranges(self.selections.disjoint_anchor_ranges())
                .map(|(snapshot, _, _)| snapshot.remote_id())
                .collect();

            for buffer_id in buffer_ids {
                if self.is_buffer_folded(buffer_id, cx) {
                    self.unfold_buffer(buffer_id, cx);
                } else {
                    self.fold_buffer(buffer_id, cx);
                }
            }
        }
    }

    pub fn toggle_fold_recursive(
        &mut self,
        _: &actions::ToggleFoldRecursive,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selection = self.selections.newest::<Point>(cx);

        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let range = if selection.is_empty() {
            let point = selection.head().to_display_point(&display_map);
            let start = DisplayPoint::new(point.row(), 0).to_point(&display_map);
            let end = DisplayPoint::new(point.row(), display_map.line_len(point.row()))
                .to_point(&display_map);
            start..end
        } else {
            selection.range()
        };
        if display_map.folds_in_range(range).next().is_some() {
            self.unfold_recursive(&Default::default(), window, cx)
        } else {
            self.fold_recursive(&Default::default(), window, cx)
        }
    }

    pub fn fold(&mut self, _: &actions::Fold, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_singleton(cx) {
            let mut to_fold = Vec::new();
            let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
            let selections = self.selections.all_adjusted(cx);

            for selection in selections {
                let range = selection.range().sorted();
                let buffer_start_row = range.start.row;

                if range.start.row != range.end.row {
                    let mut found = false;
                    let mut row = range.start.row;
                    while row <= range.end.row {
                        if let Some(crease) = display_map.crease_for_buffer_row(MultiBufferRow(row))
                        {
                            found = true;
                            row = crease.range().end.row + 1;
                            to_fold.push(crease);
                        } else {
                            row += 1
                        }
                    }
                    if found {
                        continue;
                    }
                }

                for row in (0..=range.start.row).rev() {
                    if let Some(crease) = display_map.crease_for_buffer_row(MultiBufferRow(row)) {
                        if crease.range().end.row >= buffer_start_row {
                            to_fold.push(crease);
                            if row <= range.start.row {
                                break;
                            }
                        }
                    }
                }
            }

            self.fold_creases(to_fold, true, window, cx);
        } else {
            let multi_buffer_snapshot = self.buffer.read(cx).snapshot(cx);

            let buffer_ids: HashSet<_> = multi_buffer_snapshot
                .ranges_to_buffer_ranges(self.selections.disjoint_anchor_ranges())
                .map(|(snapshot, _, _)| snapshot.remote_id())
                .collect();
            for buffer_id in buffer_ids {
                self.fold_buffer(buffer_id, cx);
            }
        }
    }

    fn fold_at_level(
        &mut self,
        fold_at: &FoldAtLevel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.buffer.read(cx).is_singleton() {
            return;
        }

        let fold_at_level = fold_at.level;
        let snapshot = self.buffer.read(cx).snapshot(cx);
        let mut to_fold = Vec::new();
        let mut stack = vec![(0, snapshot.max_row().0, 1)];

        while let Some((mut start_row, end_row, current_level)) = stack.pop() {
            while start_row < end_row {
                match self
                    .snapshot(window, cx)
                    .crease_for_buffer_row(MultiBufferRow(start_row))
                {
                    Some(crease) => {
                        let nested_start_row = crease.range().start.row + 1;
                        let nested_end_row = crease.range().end.row;

                        if current_level < fold_at_level {
                            stack.push((nested_start_row, nested_end_row, current_level + 1));
                        } else if current_level == fold_at_level {
                            to_fold.push(crease);
                        }

                        start_row = nested_end_row + 1;
                    }
                    None => start_row += 1,
                }
            }
        }

        self.fold_creases(to_fold, true, window, cx);
    }

    pub fn fold_all(&mut self, _: &actions::FoldAll, window: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.read(cx).is_singleton() {
            let mut fold_ranges = Vec::new();
            let snapshot = self.buffer.read(cx).snapshot(cx);

            for row in 0..snapshot.max_row().0 {
                if let Some(foldable_range) = self
                    .snapshot(window, cx)
                    .crease_for_buffer_row(MultiBufferRow(row))
                {
                    fold_ranges.push(foldable_range);
                }
            }

            self.fold_creases(fold_ranges, true, window, cx);
        } else {
            self.toggle_fold_multiple_buffers = cx.spawn_in(window, |editor, mut cx| async move {
                editor
                    .update_in(&mut cx, |editor, _, cx| {
                        for buffer_id in editor.buffer.read(cx).excerpt_buffer_ids() {
                            editor.fold_buffer(buffer_id, cx);
                        }
                    })
                    .ok();
            });
        }
    }

    pub fn fold_function_bodies(
        &mut self,
        _: &actions::FoldFunctionBodies,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.buffer.read(cx).snapshot(cx);

        let ranges = snapshot
            .text_object_ranges(0..snapshot.len(), TreeSitterOptions::default())
            .filter_map(|(range, obj)| (obj == TextObject::InsideFunction).then_some(range))
            .collect::<Vec<_>>();

        let creases = ranges
            .into_iter()
            .map(|range| Crease::simple(range, self.display_map.read(cx).fold_placeholder.clone()))
            .collect();

        self.fold_creases(creases, true, window, cx);
    }

    pub fn fold_recursive(
        &mut self,
        _: &actions::FoldRecursive,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut to_fold = Vec::new();
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let selections = self.selections.all_adjusted(cx);

        for selection in selections {
            let range = selection.range().sorted();
            let buffer_start_row = range.start.row;

            if range.start.row != range.end.row {
                let mut found = false;
                for row in range.start.row..=range.end.row {
                    if let Some(crease) = display_map.crease_for_buffer_row(MultiBufferRow(row)) {
                        found = true;
                        to_fold.push(crease);
                    }
                }
                if found {
                    continue;
                }
            }

            for row in (0..=range.start.row).rev() {
                if let Some(crease) = display_map.crease_for_buffer_row(MultiBufferRow(row)) {
                    if crease.range().end.row >= buffer_start_row {
                        to_fold.push(crease);
                    } else {
                        break;
                    }
                }
            }
        }

        self.fold_creases(to_fold, true, window, cx);
    }

    pub fn fold_at(&mut self, fold_at: &FoldAt, window: &mut Window, cx: &mut Context<Self>) {
        let buffer_row = fold_at.buffer_row;
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));

        if let Some(crease) = display_map.crease_for_buffer_row(buffer_row) {
            let autoscroll = self
                .selections
                .all::<Point>(cx)
                .iter()
                .any(|selection| crease.range().overlaps(&selection.range()));

            self.fold_creases(vec![crease], autoscroll, window, cx);
        }
    }

    pub fn unfold_lines(&mut self, _: &UnfoldLines, _window: &mut Window, cx: &mut Context<Self>) {
        if self.is_singleton(cx) {
            let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
            let buffer = &display_map.buffer_snapshot;
            let selections = self.selections.all::<Point>(cx);
            let ranges = selections
                .iter()
                .map(|s| {
                    let range = s.display_range(&display_map).sorted();
                    let mut start = range.start.to_point(&display_map);
                    let mut end = range.end.to_point(&display_map);
                    start.column = 0;
                    end.column = buffer.line_len(MultiBufferRow(end.row));
                    start..end
                })
                .collect::<Vec<_>>();

            self.unfold_ranges(&ranges, true, true, cx);
        } else {
            let multi_buffer_snapshot = self.buffer.read(cx).snapshot(cx);
            let buffer_ids: HashSet<_> = multi_buffer_snapshot
                .ranges_to_buffer_ranges(self.selections.disjoint_anchor_ranges())
                .map(|(snapshot, _, _)| snapshot.remote_id())
                .collect();
            for buffer_id in buffer_ids {
                self.unfold_buffer(buffer_id, cx);
            }
        }
    }

    pub fn unfold_recursive(
        &mut self,
        _: &UnfoldRecursive,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let selections = self.selections.all::<Point>(cx);
        let ranges = selections
            .iter()
            .map(|s| {
                let mut range = s.display_range(&display_map).sorted();
                *range.start.column_mut() = 0;
                *range.end.column_mut() = display_map.line_len(range.end.row());
                let start = range.start.to_point(&display_map);
                let end = range.end.to_point(&display_map);
                start..end
            })
            .collect::<Vec<_>>();

        self.unfold_ranges(&ranges, true, true, cx);
    }

    pub fn unfold_at(
        &mut self,
        unfold_at: &UnfoldAt,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));

        let intersection_range = Point::new(unfold_at.buffer_row.0, 0)
            ..Point::new(
                unfold_at.buffer_row.0,
                display_map.buffer_snapshot.line_len(unfold_at.buffer_row),
            );

        let autoscroll = self
            .selections
            .all::<Point>(cx)
            .iter()
            .any(|selection| RangeExt::overlaps(&selection.range(), &intersection_range));

        self.unfold_ranges(&[intersection_range], true, autoscroll, cx);
    }

    pub fn unfold_all(
        &mut self,
        _: &actions::UnfoldAll,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.buffer.read(cx).is_singleton() {
            let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
            self.unfold_ranges(&[0..display_map.buffer_snapshot.len()], true, true, cx);
        } else {
            self.toggle_fold_multiple_buffers = cx.spawn(|editor, mut cx| async move {
                editor
                    .update(&mut cx, |editor, cx| {
                        for buffer_id in editor.buffer.read(cx).excerpt_buffer_ids() {
                            editor.unfold_buffer(buffer_id, cx);
                        }
                    })
                    .ok();
            });
        }
    }

    pub fn fold_selected_ranges(
        &mut self,
        _: &FoldSelectedRanges,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selections = self.selections.all::<Point>(cx);
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let line_mode = self.selections.line_mode;
        let ranges = selections
            .into_iter()
            .map(|s| {
                if line_mode {
                    let start = Point::new(s.start.row, 0);
                    let end = Point::new(
                        s.end.row,
                        display_map
                            .buffer_snapshot
                            .line_len(MultiBufferRow(s.end.row)),
                    );
                    Crease::simple(start..end, display_map.fold_placeholder.clone())
                } else {
                    Crease::simple(s.start..s.end, display_map.fold_placeholder.clone())
                }
            })
            .collect::<Vec<_>>();
        self.fold_creases(ranges, true, window, cx);
    }

    pub fn fold_ranges<T: ToOffset + Clone>(
        &mut self,
        ranges: Vec<Range<T>>,
        auto_scroll: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let ranges = ranges
            .into_iter()
            .map(|r| Crease::simple(r, display_map.fold_placeholder.clone()))
            .collect::<Vec<_>>();
        self.fold_creases(ranges, auto_scroll, window, cx);
    }

    pub fn fold_creases<T: ToOffset + Clone>(
        &mut self,
        creases: Vec<Crease<T>>,
        auto_scroll: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if creases.is_empty() {
            return;
        }

        let mut buffers_affected = HashSet::default();
        let multi_buffer = self.buffer().read(cx);
        for crease in &creases {
            if let Some((_, buffer, _)) =
                multi_buffer.excerpt_containing(crease.range().start.clone(), cx)
            {
                buffers_affected.insert(buffer.read(cx).remote_id());
            };
        }

        self.display_map.update(cx, |map, cx| map.fold(creases, cx));

        if auto_scroll {
            self.request_autoscroll(Autoscroll::fit(), cx);
        }

        cx.notify();

        if let Some(active_diagnostics) = self.active_diagnostics.take() {
            // Clear diagnostics block when folding a range that contains it.
            let snapshot = self.snapshot(window, cx);
            if snapshot.intersects_fold(active_diagnostics.primary_range.start) {
                drop(snapshot);
                self.active_diagnostics = Some(active_diagnostics);
                self.dismiss_diagnostics(cx);
            } else {
                self.active_diagnostics = Some(active_diagnostics);
            }
        }

        self.scrollbar_marker_state.dirty = true;
    }

    /// Removes any folds whose ranges intersect any of the given ranges.
    pub fn unfold_ranges<T: ToOffset + Clone>(
        &mut self,
        ranges: &[Range<T>],
        inclusive: bool,
        auto_scroll: bool,
        cx: &mut Context<Self>,
    ) {
        self.remove_folds_with(ranges, auto_scroll, cx, |map, cx| {
            map.unfold_intersecting(ranges.iter().cloned(), inclusive, cx)
        });
    }

    pub fn fold_buffer(&mut self, buffer_id: BufferId, cx: &mut Context<Self>) {
        if self.buffer().read(cx).is_singleton() || self.is_buffer_folded(buffer_id, cx) {
            return;
        }
        let folded_excerpts = self.buffer().read(cx).excerpts_for_buffer(buffer_id, cx);
        self.display_map
            .update(cx, |display_map, cx| display_map.fold_buffer(buffer_id, cx));
        cx.emit(EditorEvent::BufferFoldToggled {
            ids: folded_excerpts.iter().map(|&(id, _)| id).collect(),
            folded: true,
        });
        cx.notify();
    }

    pub fn unfold_buffer(&mut self, buffer_id: BufferId, cx: &mut Context<Self>) {
        if self.buffer().read(cx).is_singleton() || !self.is_buffer_folded(buffer_id, cx) {
            return;
        }
        let unfolded_excerpts = self.buffer().read(cx).excerpts_for_buffer(buffer_id, cx);
        self.display_map.update(cx, |display_map, cx| {
            display_map.unfold_buffer(buffer_id, cx);
        });
        cx.emit(EditorEvent::BufferFoldToggled {
            ids: unfolded_excerpts.iter().map(|&(id, _)| id).collect(),
            folded: false,
        });
        cx.notify();
    }

    pub fn is_buffer_folded(&self, buffer: BufferId, cx: &App) -> bool {
        self.display_map.read(cx).is_buffer_folded(buffer)
    }

    pub fn folded_buffers<'a>(&self, cx: &'a App) -> &'a HashSet<BufferId> {
        self.display_map.read(cx).folded_buffers()
    }

    /// Removes any folds with the given ranges.
    pub fn remove_folds_with_type<T: ToOffset + Clone>(
        &mut self,
        ranges: &[Range<T>],
        type_id: TypeId,
        auto_scroll: bool,
        cx: &mut Context<Self>,
    ) {
        self.remove_folds_with(ranges, auto_scroll, cx, |map, cx| {
            map.remove_folds_with_type(ranges.iter().cloned(), type_id, cx)
        });
    }

    fn remove_folds_with<T: ToOffset + Clone>(
        &mut self,
        ranges: &[Range<T>],
        auto_scroll: bool,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut DisplayMap, &mut Context<DisplayMap>),
    ) {
        if ranges.is_empty() {
            return;
        }

        let mut buffers_affected = HashSet::default();
        let multi_buffer = self.buffer().read(cx);
        for range in ranges {
            if let Some((_, buffer, _)) = multi_buffer.excerpt_containing(range.start.clone(), cx) {
                buffers_affected.insert(buffer.read(cx).remote_id());
            };
        }

        self.display_map.update(cx, update);

        if auto_scroll {
            self.request_autoscroll(Autoscroll::fit(), cx);
        }

        cx.notify();
        self.scrollbar_marker_state.dirty = true;
        self.active_indent_guides_state.dirty = true;
    }

    pub fn default_fold_placeholder(&self, cx: &App) -> FoldPlaceholder {
        self.display_map.read(cx).fold_placeholder.clone()
    }

    pub fn set_expand_all_diff_hunks(&mut self, cx: &mut App) {
        self.buffer.update(cx, |buffer, cx| {
            buffer.set_all_diff_hunks_expanded(cx);
        });
    }

    pub fn expand_all_diff_hunks(
        &mut self,
        _: &ExpandAllHunkDiffs,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer.update(cx, |buffer, cx| {
            buffer.expand_diff_hunks(vec![Anchor::min()..Anchor::max()], cx)
        });
    }

    pub fn toggle_selected_diff_hunks(
        &mut self,
        _: &ToggleSelectedDiffHunks,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ranges: Vec<_> = self.selections.disjoint.iter().map(|s| s.range()).collect();
        self.toggle_diff_hunks_in_ranges(ranges, cx);
    }

    pub fn expand_selected_diff_hunks(&mut self, cx: &mut Context<Self>) {
        let ranges: Vec<_> = self.selections.disjoint.iter().map(|s| s.range()).collect();
        self.buffer
            .update(cx, |buffer, cx| buffer.expand_diff_hunks(ranges, cx))
    }

    pub fn clear_expanded_diff_hunks(&mut self, cx: &mut Context<Self>) -> bool {
        self.buffer.update(cx, |buffer, cx| {
            let ranges = vec![Anchor::min()..Anchor::max()];
            if !buffer.all_diff_hunks_expanded()
                && buffer.has_expanded_diff_hunks_in_ranges(&ranges, cx)
            {
                buffer.collapse_diff_hunks(ranges, cx);
                true
            } else {
                false
            }
        })
    }

    fn toggle_diff_hunks_in_ranges(
        &mut self,
        ranges: Vec<Range<Anchor>>,
        cx: &mut Context<'_, Editor>,
    ) {
        self.buffer.update(cx, |buffer, cx| {
            if buffer.has_expanded_diff_hunks_in_ranges(&ranges, cx) {
                buffer.collapse_diff_hunks(ranges, cx)
            } else {
                buffer.expand_diff_hunks(ranges, cx)
            }
        })
    }

    pub(crate) fn apply_all_diff_hunks(
        &mut self,
        _: &ApplyAllDiffHunks,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let buffers = self.buffer.read(cx).all_buffers();
        for branch_buffer in buffers {
            branch_buffer.update(cx, |branch_buffer, cx| {
                branch_buffer.merge_into_base(Vec::new(), cx);
            });
        }

        if let Some(project) = self.project.clone() {
            self.save(true, project, window, cx).detach_and_log_err(cx);
        }
    }

    pub(crate) fn apply_selected_diff_hunks(
        &mut self,
        _: &ApplyDiffHunk,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.snapshot(window, cx);
        let hunks = snapshot.hunks_for_ranges(self.selections.ranges(cx).into_iter());
        let mut ranges_by_buffer = HashMap::default();
        self.transact(window, cx, |editor, _window, cx| {
            for hunk in hunks {
                if let Some(buffer) = editor.buffer.read(cx).buffer(hunk.buffer_id) {
                    ranges_by_buffer
                        .entry(buffer.clone())
                        .or_insert_with(Vec::new)
                        .push(hunk.buffer_range.to_offset(buffer.read(cx)));
                }
            }

            for (buffer, ranges) in ranges_by_buffer {
                buffer.update(cx, |buffer, cx| {
                    buffer.merge_into_base(ranges, cx);
                });
            }
        });

        if let Some(project) = self.project.clone() {
            self.save(true, project, window, cx).detach_and_log_err(cx);
        }
    }

    pub fn set_gutter_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if hovered != self.gutter_hovered {
            self.gutter_hovered = hovered;
            cx.notify();
        }
    }

    pub fn insert_blocks(
        &mut self,
        blocks: impl IntoIterator<Item = BlockProperties<Anchor>>,
        autoscroll: Option<Autoscroll>,
        cx: &mut Context<Self>,
    ) -> Vec<CustomBlockId> {
        let blocks = self
            .display_map
            .update(cx, |display_map, cx| display_map.insert_blocks(blocks, cx));
        if let Some(autoscroll) = autoscroll {
            self.request_autoscroll(autoscroll, cx);
        }
        cx.notify();
        blocks
    }

    pub fn resize_blocks(
        &mut self,
        heights: HashMap<CustomBlockId, u32>,
        autoscroll: Option<Autoscroll>,
        cx: &mut Context<Self>,
    ) {
        self.display_map
            .update(cx, |display_map, cx| display_map.resize_blocks(heights, cx));
        if let Some(autoscroll) = autoscroll {
            self.request_autoscroll(autoscroll, cx);
        }
        cx.notify();
    }

    pub fn replace_blocks(
        &mut self,
        renderers: HashMap<CustomBlockId, RenderBlock>,
        autoscroll: Option<Autoscroll>,
        cx: &mut Context<Self>,
    ) {
        self.display_map
            .update(cx, |display_map, _cx| display_map.replace_blocks(renderers));
        if let Some(autoscroll) = autoscroll {
            self.request_autoscroll(autoscroll, cx);
        }
        cx.notify();
    }

    pub fn remove_blocks(
        &mut self,
        block_ids: HashSet<CustomBlockId>,
        autoscroll: Option<Autoscroll>,
        cx: &mut Context<Self>,
    ) {
        self.display_map.update(cx, |display_map, cx| {
            display_map.remove_blocks(block_ids, cx)
        });
        if let Some(autoscroll) = autoscroll {
            self.request_autoscroll(autoscroll, cx);
        }
        cx.notify();
    }

    pub fn row_for_block(
        &self,
        block_id: CustomBlockId,
        cx: &mut Context<Self>,
    ) -> Option<DisplayRow> {
        self.display_map
            .update(cx, |map, cx| map.row_for_block(block_id, cx))
    }

    pub(crate) fn set_focused_block(&mut self, focused_block: FocusedBlock) {
        self.focused_block = Some(focused_block);
    }

    pub(crate) fn take_focused_block(&mut self) -> Option<FocusedBlock> {
        self.focused_block.take()
    }

    pub fn insert_creases(
        &mut self,
        creases: impl IntoIterator<Item = Crease<Anchor>>,
        cx: &mut Context<Self>,
    ) -> Vec<CreaseId> {
        self.display_map
            .update(cx, |map, cx| map.insert_creases(creases, cx))
    }

    pub fn remove_creases(
        &mut self,
        ids: impl IntoIterator<Item = CreaseId>,
        cx: &mut Context<Self>,
    ) {
        self.display_map
            .update(cx, |map, cx| map.remove_creases(ids, cx));
    }

    pub fn longest_row(&self, cx: &mut App) -> DisplayRow {
        self.display_map
            .update(cx, |map, cx| map.snapshot(cx))
            .longest_row()
    }

    pub fn max_point(&self, cx: &mut App) -> DisplayPoint {
        self.display_map
            .update(cx, |map, cx| map.snapshot(cx))
            .max_point()
    }

    pub fn text(&self, cx: &App) -> String {
        self.buffer.read(cx).read(cx).text()
    }

    pub fn is_empty(&self, cx: &App) -> bool {
        self.buffer.read(cx).read(cx).is_empty()
    }

    pub fn text_option(&self, cx: &App) -> Option<String> {
        let text = self.text(cx);
        let text = text.trim();

        if text.is_empty() {
            return None;
        }

        Some(text.to_string())
    }

    pub fn set_text(
        &mut self,
        text: impl Into<Arc<str>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.transact(window, cx, |this, _, cx| {
            this.buffer
                .read(cx)
                .as_singleton()
                .expect("you can only call set_text on editors for singleton buffers")
                .update(cx, |buffer, cx| buffer.set_text(text, cx));
        });
    }

    pub fn display_text(&self, cx: &mut App) -> String {
        self.display_map
            .update(cx, |map, cx| map.snapshot(cx))
            .text()
    }

    pub fn wrap_guides(&self, cx: &App) -> SmallVec<[(usize, bool); 2]> {
        let mut wrap_guides = smallvec::smallvec![];

        if self.show_wrap_guides == Some(false) {
            return wrap_guides;
        }

        let settings = self.buffer.read(cx).settings_at(0, cx);
        if settings.show_wrap_guides {
            if let SoftWrap::Column(soft_wrap) = self.soft_wrap_mode(cx) {
                wrap_guides.push((soft_wrap as usize, true));
            } else if let SoftWrap::Bounded(soft_wrap) = self.soft_wrap_mode(cx) {
                wrap_guides.push((soft_wrap as usize, true));
            }
            wrap_guides.extend(settings.wrap_guides.iter().map(|guide| (*guide, false)))
        }

        wrap_guides
    }

    pub fn soft_wrap_mode(&self, cx: &App) -> SoftWrap {
        let settings = self.buffer.read(cx).settings_at(0, cx);
        let mode = self.soft_wrap_mode_override.unwrap_or(settings.soft_wrap);
        match mode {
            language_settings::SoftWrap::PreferLine | language_settings::SoftWrap::None => {
                SoftWrap::None
            }
            language_settings::SoftWrap::EditorWidth => SoftWrap::EditorWidth,
            language_settings::SoftWrap::PreferredLineLength => {
                SoftWrap::Column(settings.preferred_line_length)
            }
            language_settings::SoftWrap::Bounded => {
                SoftWrap::Bounded(settings.preferred_line_length)
            }
        }
    }

    pub fn set_soft_wrap_mode(
        &mut self,
        mode: language_settings::SoftWrap,

        cx: &mut Context<Self>,
    ) {
        self.soft_wrap_mode_override = Some(mode);
        cx.notify();
    }

    pub fn set_text_style_refinement(&mut self, style: TextStyleRefinement) {
        self.text_style_refinement = Some(style);
    }

    /// called by the Element so we know what style we were most recently rendered with.
    pub(crate) fn set_style(
        &mut self,
        style: EditorStyle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rem_size = window.rem_size();
        self.display_map.update(cx, |map, cx| {
            map.set_font(
                style.text.font(),
                style.text.font_size.to_pixels(rem_size),
                cx,
            )
        });
        self.style = Some(style);
    }

    pub fn style(&self) -> Option<&EditorStyle> {
        self.style.as_ref()
    }

    // Called by the element. This method is not designed to be called outside of the editor
    // element's layout code because it does not notify when rewrapping is computed synchronously.
    pub(crate) fn set_wrap_width(&self, width: Option<Pixels>, cx: &mut App) -> bool {
        self.display_map
            .update(cx, |map, cx| map.set_wrap_width(width, cx))
    }

    pub fn set_soft_wrap(&mut self) {
        self.soft_wrap_mode_override = Some(language_settings::SoftWrap::EditorWidth)
    }

    pub fn toggle_soft_wrap(&mut self, _: &ToggleSoftWrap, _: &mut Window, cx: &mut Context<Self>) {
        if self.soft_wrap_mode_override.is_some() {
            self.soft_wrap_mode_override.take();
        } else {
            let soft_wrap = match self.soft_wrap_mode(cx) {
                SoftWrap::GitDiff => return,
                SoftWrap::None => language_settings::SoftWrap::EditorWidth,
                SoftWrap::EditorWidth | SoftWrap::Column(_) | SoftWrap::Bounded(_) => {
                    language_settings::SoftWrap::None
                }
            };
            self.soft_wrap_mode_override = Some(soft_wrap);
        }
        cx.notify();
    }

    pub fn toggle_tab_bar(&mut self, _: &ToggleTabBar, _: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace() else {
            return;
        };
        let fs = workspace.read(cx).app_state().fs.clone();
        let current_show = TabBarSettings::get_global(cx).show;
        update_settings_file::<TabBarSettings>(fs, cx, move |setting, _| {
            setting.show = Some(!current_show);
        });
    }

    pub fn toggle_indent_guides(
        &mut self,
        _: &ToggleIndentGuides,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let currently_enabled = self.should_show_indent_guides().unwrap_or_else(|| {
            self.buffer
                .read(cx)
                .settings_at(0, cx)
                .indent_guides
                .enabled
        });
        self.show_indent_guides = Some(!currently_enabled);
        cx.notify();
    }

    fn should_show_indent_guides(&self) -> Option<bool> {
        self.show_indent_guides
    }

    pub fn toggle_line_numbers(
        &mut self,
        _: &ToggleLineNumbers,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut editor_settings = EditorSettings::get_global(cx).clone();
        editor_settings.gutter.line_numbers = !editor_settings.gutter.line_numbers;
        EditorSettings::override_global(editor_settings, cx);
    }

    pub fn should_use_relative_line_numbers(&self, cx: &mut App) -> bool {
        self.use_relative_line_numbers
            .unwrap_or(EditorSettings::get_global(cx).relative_line_numbers)
    }

    pub fn toggle_relative_line_numbers(
        &mut self,
        _: &ToggleRelativeLineNumbers,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_relative = self.should_use_relative_line_numbers(cx);
        self.set_relative_line_number(Some(!is_relative), cx)
    }

    pub fn set_relative_line_number(&mut self, is_relative: Option<bool>, cx: &mut Context<Self>) {
        self.use_relative_line_numbers = is_relative;
        cx.notify();
    }

    pub fn set_show_gutter(&mut self, show_gutter: bool, cx: &mut Context<Self>) {
        self.show_gutter = show_gutter;
        cx.notify();
    }

    pub fn set_show_scrollbars(&mut self, show_scrollbars: bool, cx: &mut Context<Self>) {
        self.show_scrollbars = show_scrollbars;
        cx.notify();
    }

    pub fn set_show_line_numbers(&mut self, show_line_numbers: bool, cx: &mut Context<Self>) {
        self.show_line_numbers = Some(show_line_numbers);
        cx.notify();
    }

    pub fn set_show_git_diff_gutter(&mut self, show_git_diff_gutter: bool, cx: &mut Context<Self>) {
        self.show_git_diff_gutter = Some(show_git_diff_gutter);
        cx.notify();
    }

    pub fn set_show_code_actions(&mut self, show_code_actions: bool, cx: &mut Context<Self>) {
        self.show_code_actions = Some(show_code_actions);
        cx.notify();
    }

    pub fn set_show_runnables(&mut self, show_runnables: bool, cx: &mut Context<Self>) {
        self.show_runnables = Some(show_runnables);
        cx.notify();
    }

    pub fn set_masked(&mut self, masked: bool, cx: &mut Context<Self>) {
        if self.display_map.read(cx).masked != masked {
            self.display_map.update(cx, |map, _| map.masked = masked);
        }
        cx.notify()
    }

    pub fn set_show_wrap_guides(&mut self, show_wrap_guides: bool, cx: &mut Context<Self>) {
        self.show_wrap_guides = Some(show_wrap_guides);
        cx.notify();
    }

    pub fn set_show_indent_guides(&mut self, show_indent_guides: bool, cx: &mut Context<Self>) {
        self.show_indent_guides = Some(show_indent_guides);
        cx.notify();
    }

    pub fn working_directory(&self, cx: &App) -> Option<PathBuf> {
        if let Some(buffer) = self.buffer().read(cx).as_singleton() {
            if let Some(file) = buffer.read(cx).file().and_then(|f| f.as_local()) {
                if let Some(dir) = file.abs_path(cx).parent() {
                    return Some(dir.to_owned());
                }
            }

            if let Some(project_path) = buffer.read(cx).project_path(cx) {
                return Some(project_path.path.to_path_buf());
            }
        }

        None
    }

    fn target_file<'a>(&self, cx: &'a App) -> Option<&'a dyn language::LocalFile> {
        self.active_excerpt(cx)?
            .1
            .read(cx)
            .file()
            .and_then(|f| f.as_local())
    }

    fn target_file_abs_path(&self, cx: &mut Context<Self>) -> Option<PathBuf> {
        self.active_excerpt(cx).and_then(|(_, buffer, _)| {
            let project_path = buffer.read(cx).project_path(cx)?;
            let project = self.project.as_ref()?.read(cx);
            project.absolute_path(&project_path, cx)
        })
    }

    fn target_file_path(&self, cx: &mut Context<Self>) -> Option<PathBuf> {
        self.active_excerpt(cx).and_then(|(_, buffer, _)| {
            let project_path = buffer.read(cx).project_path(cx)?;
            let project = self.project.as_ref()?.read(cx);
            let entry = project.entry_for_path(&project_path, cx)?;
            let path = entry.path.to_path_buf();
            Some(path)
        })
    }

    pub fn reveal_in_finder(
        &mut self,
        _: &RevealInFileManager,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(target) = self.target_file(cx) {
            cx.reveal_path(&target.abs_path(cx));
        }
    }

    pub fn copy_path(&mut self, _: &CopyPath, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.target_file_abs_path(cx) {
            if let Some(path) = path.to_str() {
                cx.write_to_clipboard(ClipboardItem::new_string(path.to_string()));
            }
        }
    }

    pub fn copy_relative_path(
        &mut self,
        _: &CopyRelativePath,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(path) = self.target_file_path(cx) {
            if let Some(path) = path.to_str() {
                cx.write_to_clipboard(ClipboardItem::new_string(path.to_string()));
            }
        }
    }

    pub fn toggle_git_blame(
        &mut self,
        _: &ToggleGitBlame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_git_blame_gutter = !self.show_git_blame_gutter;

        if self.show_git_blame_gutter && !self.has_blame_entries(cx) {
            self.start_git_blame(true, window, cx);
        }

        cx.notify();
    }

    pub fn toggle_git_blame_inline(
        &mut self,
        _: &ToggleGitBlameInline,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_git_blame_inline_internal(true, window, cx);
        cx.notify();
    }

    pub fn git_blame_inline_enabled(&self) -> bool {
        self.git_blame_inline_enabled
    }

    pub fn toggle_selection_menu(
        &mut self,
        _: &ToggleSelectionMenu,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_selection_menu = self
            .show_selection_menu
            .map(|show_selections_menu| !show_selections_menu)
            .or_else(|| Some(!EditorSettings::get_global(cx).toolbar.selections_menu));

        cx.notify();
    }

    pub fn selection_menu_enabled(&self, cx: &App) -> bool {
        self.show_selection_menu
            .unwrap_or_else(|| EditorSettings::get_global(cx).toolbar.selections_menu)
    }

    fn start_git_blame(
        &mut self,
        user_triggered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(project) = self.project.as_ref() {
            let Some(buffer) = self.buffer().read(cx).as_singleton() else {
                return;
            };

            if buffer.read(cx).file().is_none() {
                return;
            }

            let focused = self.focus_handle(cx).contains_focused(window, cx);

            let project = project.clone();
            let blame = cx.new(|cx| GitBlame::new(buffer, project, user_triggered, focused, cx));
            self.blame_subscription =
                Some(cx.observe_in(&blame, window, |_, _, _, cx| cx.notify()));
            self.blame = Some(blame);
        }
    }

    fn toggle_git_blame_inline_internal(
        &mut self,
        user_triggered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.git_blame_inline_enabled {
            self.git_blame_inline_enabled = false;
            self.show_git_blame_inline = false;
            self.show_git_blame_inline_delay_task.take();
        } else {
            self.git_blame_inline_enabled = true;
            self.start_git_blame_inline(user_triggered, window, cx);
        }

        cx.notify();
    }

    fn start_git_blame_inline(
        &mut self,
        user_triggered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_git_blame(user_triggered, window, cx);

        if ProjectSettings::get_global(cx)
            .git
            .inline_blame_delay()
            .is_some()
        {
            self.start_inline_blame_timer(window, cx);
        } else {
            self.show_git_blame_inline = true
        }
    }

    pub fn blame(&self) -> Option<&Entity<GitBlame>> {
        self.blame.as_ref()
    }

    pub fn show_git_blame_gutter(&self) -> bool {
        self.show_git_blame_gutter
    }

    pub fn render_git_blame_gutter(&self, cx: &App) -> bool {
        self.show_git_blame_gutter && self.has_blame_entries(cx)
    }

    pub fn render_git_blame_inline(&self, window: &Window, cx: &App) -> bool {
        self.show_git_blame_inline
            && self.focus_handle.is_focused(window)
            && !self.newest_selection_head_on_empty_line(cx)
            && self.has_blame_entries(cx)
    }

    fn has_blame_entries(&self, cx: &App) -> bool {
        self.blame()
            .map_or(false, |blame| blame.read(cx).has_generated_entries())
    }

    fn newest_selection_head_on_empty_line(&self, cx: &App) -> bool {
        let cursor_anchor = self.selections.newest_anchor().head();

        let snapshot = self.buffer.read(cx).snapshot(cx);
        let buffer_row = MultiBufferRow(cursor_anchor.to_point(&snapshot).row);

        snapshot.line_len(buffer_row) == 0
    }

    fn get_permalink_to_line(&self, cx: &mut Context<Self>) -> Task<Result<url::Url>> {
        let buffer_and_selection = maybe!({
            let selection = self.selections.newest::<Point>(cx);
            let selection_range = selection.range();

            let multi_buffer = self.buffer().read(cx);
            let multi_buffer_snapshot = multi_buffer.snapshot(cx);
            let buffer_ranges = multi_buffer_snapshot.range_to_buffer_ranges(selection_range);

            let (buffer, range, _) = if selection.reversed {
                buffer_ranges.first()
            } else {
                buffer_ranges.last()
            }?;

            let selection = text::ToPoint::to_point(&range.start, &buffer).row
                ..text::ToPoint::to_point(&range.end, &buffer).row;
            Some((
                multi_buffer.buffer(buffer.remote_id()).unwrap().clone(),
                selection,
            ))
        });

        let Some((buffer, selection)) = buffer_and_selection else {
            return Task::ready(Err(anyhow!("failed to determine buffer and selection")));
        };

        let Some(project) = self.project.as_ref() else {
            return Task::ready(Err(anyhow!("editor does not have project")));
        };

        project.update(cx, |project, cx| {
            project.get_permalink_to_line(&buffer, selection, cx)
        })
    }

    pub fn copy_permalink_to_line(
        &mut self,
        _: &CopyPermalinkToLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let permalink_task = self.get_permalink_to_line(cx);
        let workspace = self.workspace();

        cx.spawn_in(window, |_, mut cx| async move {
            match permalink_task.await {
                Ok(permalink) => {
                    cx.update(|_, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(permalink.to_string()));
                    })
                    .ok();
                }
                Err(err) => {
                    let message = format!("Failed to copy permalink: {err}");

                    Err::<(), anyhow::Error>(err).log_err();

                    if let Some(workspace) = workspace {
                        workspace
                            .update_in(&mut cx, |workspace, _, cx| {
                                struct CopyPermalinkToLine;

                                workspace.show_toast(
                                    Toast::new(
                                        NotificationId::unique::<CopyPermalinkToLine>(),
                                        message,
                                    ),
                                    cx,
                                )
                            })
                            .ok();
                    }
                }
            }
        })
        .detach();
    }

    pub fn copy_file_location(
        &mut self,
        _: &CopyFileLocation,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selection = self.selections.newest::<Point>(cx).start.row + 1;
        if let Some(file) = self.target_file(cx) {
            if let Some(path) = file.path().to_str() {
                cx.write_to_clipboard(ClipboardItem::new_string(format!("{path}:{selection}")));
            }
        }
    }

    pub fn open_permalink_to_line(
        &mut self,
        _: &OpenPermalinkToLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let permalink_task = self.get_permalink_to_line(cx);
        let workspace = self.workspace();

        cx.spawn_in(window, |_, mut cx| async move {
            match permalink_task.await {
                Ok(permalink) => {
                    cx.update(|_, cx| {
                        cx.open_url(permalink.as_ref());
                    })
                    .ok();
                }
                Err(err) => {
                    let message = format!("Failed to open permalink: {err}");

                    Err::<(), anyhow::Error>(err).log_err();

                    if let Some(workspace) = workspace {
                        workspace
                            .update(&mut cx, |workspace, cx| {
                                struct OpenPermalinkToLine;

                                workspace.show_toast(
                                    Toast::new(
                                        NotificationId::unique::<OpenPermalinkToLine>(),
                                        message,
                                    ),
                                    cx,
                                )
                            })
                            .ok();
                    }
                }
            }
        })
        .detach();
    }

    pub fn insert_uuid_v4(
        &mut self,
        _: &InsertUuidV4,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_uuid(UuidVersion::V4, window, cx);
    }

    pub fn insert_uuid_v7(
        &mut self,
        _: &InsertUuidV7,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_uuid(UuidVersion::V7, window, cx);
    }

    fn insert_uuid(&mut self, version: UuidVersion, window: &mut Window, cx: &mut Context<Self>) {
        self.transact(window, cx, |this, window, cx| {
            let edits = this
                .selections
                .all::<Point>(cx)
                .into_iter()
                .map(|selection| {
                    let uuid = match version {
                        UuidVersion::V4 => uuid::Uuid::new_v4(),
                        UuidVersion::V7 => uuid::Uuid::now_v7(),
                    };

                    (selection.range(), uuid.to_string())
                });
            this.edit(edits, cx);
            this.refresh_inline_completion(true, false, window, cx);
        });
    }

    pub fn open_selections_in_multibuffer(
        &mut self,
        _: &OpenSelectionsInMultibuffer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let multibuffer = self.buffer.read(cx);

        let Some(buffer) = multibuffer.as_singleton() else {
            return;
        };

        let Some(workspace) = self.workspace() else {
            return;
        };

        let locations = self
            .selections
            .disjoint_anchors()
            .iter()
            .map(|range| Location {
                buffer: buffer.clone(),
                range: range.start.text_anchor..range.end.text_anchor,
            })
            .collect::<Vec<_>>();

        let title = multibuffer.title(cx).to_string();

        cx.spawn_in(window, |_, mut cx| async move {
            workspace.update_in(&mut cx, |workspace, window, cx| {
                Self::open_locations_in_multibuffer(
                    workspace,
                    locations,
                    format!("Selections for '{title}'"),
                    false,
                    MultibufferSelectionMode::All,
                    window,
                    cx,
                );
            })
        })
        .detach();
    }

    /// Adds a row highlight for the given range. If a row has multiple highlights, the
    /// last highlight added will be used.
    ///
    /// If the range ends at the beginning of a line, then that line will not be highlighted.
    pub fn highlight_rows<T: 'static>(
        &mut self,
        range: Range<Anchor>,
        color: Hsla,
        should_autoscroll: bool,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.buffer().read(cx).snapshot(cx);
        let row_highlights = self.highlighted_rows.entry(TypeId::of::<T>()).or_default();
        let ix = row_highlights.binary_search_by(|highlight| {
            Ordering::Equal
                .then_with(|| highlight.range.start.cmp(&range.start, &snapshot))
                .then_with(|| highlight.range.end.cmp(&range.end, &snapshot))
        });

        if let Err(mut ix) = ix {
            let index = post_inc(&mut self.highlight_order);

            // If this range intersects with the preceding highlight, then merge it with
            // the preceding highlight. Otherwise insert a new highlight.
            let mut merged = false;
            if ix > 0 {
                let prev_highlight = &mut row_highlights[ix - 1];
                if prev_highlight
                    .range
                    .end
                    .cmp(&range.start, &snapshot)
                    .is_ge()
                {
                    ix -= 1;
                    if prev_highlight.range.end.cmp(&range.end, &snapshot).is_lt() {
                        prev_highlight.range.end = range.end;
                    }
                    merged = true;
                    prev_highlight.index = index;
                    prev_highlight.color = color;
                    prev_highlight.should_autoscroll = should_autoscroll;
                }
            }

            if !merged {
                row_highlights.insert(
                    ix,
                    RowHighlight {
                        range: range.clone(),
                        index,
                        color,
                        should_autoscroll,
                    },
                );
            }

            // If any of the following highlights intersect with this one, merge them.
            while let Some(next_highlight) = row_highlights.get(ix + 1) {
                let highlight = &row_highlights[ix];
                if next_highlight
                    .range
                    .start
                    .cmp(&highlight.range.end, &snapshot)
                    .is_le()
                {
                    if next_highlight
                        .range
                        .end
                        .cmp(&highlight.range.end, &snapshot)
                        .is_gt()
                    {
                        row_highlights[ix].range.end = next_highlight.range.end;
                    }
                    row_highlights.remove(ix + 1);
                } else {
                    break;
                }
            }
        }
    }

    /// Remove any highlighted row ranges of the given type that intersect the
    /// given ranges.
    pub fn remove_highlighted_rows<T: 'static>(
        &mut self,
        ranges_to_remove: Vec<Range<Anchor>>,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.buffer().read(cx).snapshot(cx);
        let row_highlights = self.highlighted_rows.entry(TypeId::of::<T>()).or_default();
        let mut ranges_to_remove = ranges_to_remove.iter().peekable();
        row_highlights.retain(|highlight| {
            while let Some(range_to_remove) = ranges_to_remove.peek() {
                match range_to_remove.end.cmp(&highlight.range.start, &snapshot) {
                    Ordering::Less | Ordering::Equal => {
                        ranges_to_remove.next();
                    }
                    Ordering::Greater => {
                        match range_to_remove.start.cmp(&highlight.range.end, &snapshot) {
                            Ordering::Less | Ordering::Equal => {
                                return false;
                            }
                            Ordering::Greater => break,
                        }
                    }
                }
            }

            true
        })
    }

    /// Clear all anchor ranges for a certain highlight context type, so no corresponding rows will be highlighted.
    pub fn clear_row_highlights<T: 'static>(&mut self) {
        self.highlighted_rows.remove(&TypeId::of::<T>());
    }

    /// For a highlight given context type, gets all anchor ranges that will be used for row highlighting.
    pub fn highlighted_rows<T: 'static>(&self) -> impl '_ + Iterator<Item = (Range<Anchor>, Hsla)> {
        self.highlighted_rows
            .get(&TypeId::of::<T>())
            .map_or(&[] as &[_], |vec| vec.as_slice())
            .iter()
            .map(|highlight| (highlight.range.clone(), highlight.color))
    }

    /// Merges all anchor ranges for all context types ever set, picking the last highlight added in case of a row conflict.
    /// Returns a map of display rows that are highlighted and their corresponding highlight color.
    /// Allows to ignore certain kinds of highlights.
    pub fn highlighted_display_rows(
        &self,
        window: &mut Window,
        cx: &mut App,
    ) -> BTreeMap<DisplayRow, Hsla> {
        let snapshot = self.snapshot(window, cx);
        let mut used_highlight_orders = HashMap::default();
        self.highlighted_rows
            .iter()
            .flat_map(|(_, highlighted_rows)| highlighted_rows.iter())
            .fold(
                BTreeMap::<DisplayRow, Hsla>::new(),
                |mut unique_rows, highlight| {
                    let start = highlight.range.start.to_display_point(&snapshot);
                    let end = highlight.range.end.to_display_point(&snapshot);
                    let start_row = start.row().0;
                    let end_row = if highlight.range.end.text_anchor != text::Anchor::MAX
                        && end.column() == 0
                    {
                        end.row().0.saturating_sub(1)
                    } else {
                        end.row().0
                    };
                    for row in start_row..=end_row {
                        let used_index =
                            used_highlight_orders.entry(row).or_insert(highlight.index);
                        if highlight.index >= *used_index {
                            *used_index = highlight.index;
                            unique_rows.insert(DisplayRow(row), highlight.color);
                        }
                    }
                    unique_rows
                },
            )
    }

    pub fn highlighted_display_row_for_autoscroll(
        &self,
        snapshot: &DisplaySnapshot,
    ) -> Option<DisplayRow> {
        self.highlighted_rows
            .values()
            .flat_map(|highlighted_rows| highlighted_rows.iter())
            .filter_map(|highlight| {
                if highlight.should_autoscroll {
                    Some(highlight.range.start.to_display_point(snapshot).row())
                } else {
                    None
                }
            })
            .min()
    }

    pub fn set_search_within_ranges(&mut self, ranges: &[Range<Anchor>], cx: &mut Context<Self>) {
        self.highlight_background::<SearchWithinRange>(
            ranges,
            |colors| colors.editor_document_highlight_read_background,
            cx,
        )
    }

    pub fn set_breadcrumb_header(&mut self, new_header: String) {
        self.breadcrumb_header = Some(new_header);
    }

    pub fn clear_search_within_ranges(&mut self, cx: &mut Context<Self>) {
        self.clear_background_highlights::<SearchWithinRange>(cx);
    }

    pub fn highlight_background<T: 'static>(
        &mut self,
        ranges: &[Range<Anchor>],
        color_fetcher: fn(&ThemeColors) -> Hsla,
        cx: &mut Context<Self>,
    ) {
        self.background_highlights
            .insert(TypeId::of::<T>(), (color_fetcher, Arc::from(ranges)));
        self.scrollbar_marker_state.dirty = true;
        cx.notify();
    }

    pub fn clear_background_highlights<T: 'static>(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<BackgroundHighlight> {
        let text_highlights = self.background_highlights.remove(&TypeId::of::<T>())?;
        if !text_highlights.1.is_empty() {
            self.scrollbar_marker_state.dirty = true;
            cx.notify();
        }
        Some(text_highlights)
    }

    pub fn highlight_gutter<T: 'static>(
        &mut self,
        ranges: &[Range<Anchor>],
        color_fetcher: fn(&App) -> Hsla,
        cx: &mut Context<Self>,
    ) {
        self.gutter_highlights
            .insert(TypeId::of::<T>(), (color_fetcher, Arc::from(ranges)));
        cx.notify();
    }

    pub fn clear_gutter_highlights<T: 'static>(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<GutterHighlight> {
        cx.notify();
        self.gutter_highlights.remove(&TypeId::of::<T>())
    }

    #[cfg(feature = "test-support")]
    pub fn all_text_background_highlights(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<(Range<DisplayPoint>, Hsla)> {
        let snapshot = self.snapshot(window, cx);
        let buffer = &snapshot.buffer_snapshot;
        let start = buffer.anchor_before(0);
        let end = buffer.anchor_after(buffer.len());
        let theme = cx.theme().colors();
        self.background_highlights_in_range(start..end, &snapshot, theme)
    }

    #[cfg(feature = "test-support")]
    pub fn search_background_highlights(&mut self, cx: &mut Context<Self>) -> Vec<Range<Point>> {
        let snapshot = self.buffer().read(cx).snapshot(cx);

        let highlights = self
            .background_highlights
            .get(&TypeId::of::<items::BufferSearchHighlights>());

        if let Some((_color, ranges)) = highlights {
            ranges
                .iter()
                .map(|range| range.start.to_point(&snapshot)..range.end.to_point(&snapshot))
                .collect_vec()
        } else {
            vec![]
        }
    }

    fn document_highlights_for_position<'a>(
        &'a self,
        position: Anchor,
        buffer: &'a MultiBufferSnapshot,
    ) -> impl 'a + Iterator<Item = &'a Range<Anchor>> {
        let read_highlights = self
            .background_highlights
            .get(&TypeId::of::<DocumentHighlightRead>())
            .map(|h| &h.1);
        let write_highlights = self
            .background_highlights
            .get(&TypeId::of::<DocumentHighlightWrite>())
            .map(|h| &h.1);
        let left_position = position.bias_left(buffer);
        let right_position = position.bias_right(buffer);
        read_highlights
            .into_iter()
            .chain(write_highlights)
            .flat_map(move |ranges| {
                let start_ix = match ranges.binary_search_by(|probe| {
                    let cmp = probe.end.cmp(&left_position, buffer);
                    if cmp.is_ge() {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    }
                }) {
                    Ok(i) | Err(i) => i,
                };

                ranges[start_ix..]
                    .iter()
                    .take_while(move |range| range.start.cmp(&right_position, buffer).is_le())
            })
    }

    pub fn has_background_highlights<T: 'static>(&self) -> bool {
        self.background_highlights
            .get(&TypeId::of::<T>())
            .map_or(false, |(_, highlights)| !highlights.is_empty())
    }

    pub fn background_highlights_in_range(
        &self,
        search_range: Range<Anchor>,
        display_snapshot: &DisplaySnapshot,
        theme: &ThemeColors,
    ) -> Vec<(Range<DisplayPoint>, Hsla)> {
        let mut results = Vec::new();
        for (color_fetcher, ranges) in self.background_highlights.values() {
            let color = color_fetcher(theme);
            let start_ix = match ranges.binary_search_by(|probe| {
                let cmp = probe
                    .end
                    .cmp(&search_range.start, &display_snapshot.buffer_snapshot);
                if cmp.is_gt() {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }) {
                Ok(i) | Err(i) => i,
            };
            for range in &ranges[start_ix..] {
                if range
                    .start
                    .cmp(&search_range.end, &display_snapshot.buffer_snapshot)
                    .is_ge()
                {
                    break;
                }

                let start = range.start.to_display_point(display_snapshot);
                let end = range.end.to_display_point(display_snapshot);
                results.push((start..end, color))
            }
        }
        results
    }

    pub fn background_highlight_row_ranges<T: 'static>(
        &self,
        search_range: Range<Anchor>,
        display_snapshot: &DisplaySnapshot,
        count: usize,
    ) -> Vec<RangeInclusive<DisplayPoint>> {
        let mut results = Vec::new();
        let Some((_, ranges)) = self.background_highlights.get(&TypeId::of::<T>()) else {
            return vec![];
        };

        let start_ix = match ranges.binary_search_by(|probe| {
            let cmp = probe
                .end
                .cmp(&search_range.start, &display_snapshot.buffer_snapshot);
            if cmp.is_gt() {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }) {
            Ok(i) | Err(i) => i,
        };
        let mut push_region = |start: Option<Point>, end: Option<Point>| {
            if let (Some(start_display), Some(end_display)) = (start, end) {
                results.push(
                    start_display.to_display_point(display_snapshot)
                        ..=end_display.to_display_point(display_snapshot),
                );
            }
        };
        let mut start_row: Option<Point> = None;
        let mut end_row: Option<Point> = None;
        if ranges.len() > count {
            return Vec::new();
        }
        for range in &ranges[start_ix..] {
            if range
                .start
                .cmp(&search_range.end, &display_snapshot.buffer_snapshot)
                .is_ge()
            {
                break;
            }
            let end = range.end.to_point(&display_snapshot.buffer_snapshot);
            if let Some(current_row) = &end_row {
                if end.row == current_row.row {
                    continue;
                }
            }
            let start = range.start.to_point(&display_snapshot.buffer_snapshot);
            if start_row.is_none() {
                assert_eq!(end_row, None);
                start_row = Some(start);
                end_row = Some(end);
                continue;
            }
            if let Some(current_end) = end_row.as_mut() {
                if start.row > current_end.row + 1 {
                    push_region(start_row, end_row);
                    start_row = Some(start);
                    end_row = Some(end);
                } else {
                    // Merge two hunks.
                    *current_end = end;
                }
            } else {
                unreachable!();
            }
        }
        // We might still have a hunk that was not rendered (if there was a search hit on the last line)
        push_region(start_row, end_row);
        results
    }

    pub fn gutter_highlights_in_range(
        &self,
        search_range: Range<Anchor>,
        display_snapshot: &DisplaySnapshot,
        cx: &App,
    ) -> Vec<(Range<DisplayPoint>, Hsla)> {
        let mut results = Vec::new();
        for (color_fetcher, ranges) in self.gutter_highlights.values() {
            let color = color_fetcher(cx);
            let start_ix = match ranges.binary_search_by(|probe| {
                let cmp = probe
                    .end
                    .cmp(&search_range.start, &display_snapshot.buffer_snapshot);
                if cmp.is_gt() {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }) {
                Ok(i) | Err(i) => i,
            };
            for range in &ranges[start_ix..] {
                if range
                    .start
                    .cmp(&search_range.end, &display_snapshot.buffer_snapshot)
                    .is_ge()
                {
                    break;
                }

                let start = range.start.to_display_point(display_snapshot);
                let end = range.end.to_display_point(display_snapshot);
                results.push((start..end, color))
            }
        }
        results
    }

    /// Get the text ranges corresponding to the redaction query
    pub fn redacted_ranges(
        &self,
        search_range: Range<Anchor>,
        display_snapshot: &DisplaySnapshot,
        cx: &App,
    ) -> Vec<Range<DisplayPoint>> {
        display_snapshot
            .buffer_snapshot
            .redacted_ranges(search_range, |file| {
                if let Some(file) = file {
                    file.is_private()
                        && EditorSettings::get(
                            Some(SettingsLocation {
                                worktree_id: file.worktree_id(cx),
                                path: file.path().as_ref(),
                            }),
                            cx,
                        )
                        .redact_private_values
                } else {
                    false
                }
            })
            .map(|range| {
                range.start.to_display_point(display_snapshot)
                    ..range.end.to_display_point(display_snapshot)
            })
            .collect()
    }

    pub fn highlight_text<T: 'static>(
        &mut self,
        ranges: Vec<Range<Anchor>>,
        style: HighlightStyle,
        cx: &mut Context<Self>,
    ) {
        self.display_map.update(cx, |map, _| {
            map.highlight_text(TypeId::of::<T>(), ranges, style)
        });
        cx.notify();
    }

    pub(crate) fn highlight_inlays<T: 'static>(
        &mut self,
        highlights: Vec<InlayHighlight>,
        style: HighlightStyle,
        cx: &mut Context<Self>,
    ) {
        self.display_map.update(cx, |map, _| {
            map.highlight_inlays(TypeId::of::<T>(), highlights, style)
        });
        cx.notify();
    }

    pub fn text_highlights<'a, T: 'static>(
        &'a self,
        cx: &'a App,
    ) -> Option<(HighlightStyle, &'a [Range<Anchor>])> {
        self.display_map.read(cx).text_highlights(TypeId::of::<T>())
    }

    pub fn clear_highlights<T: 'static>(&mut self, cx: &mut Context<Self>) {
        let cleared = self
            .display_map
            .update(cx, |map, _| map.clear_highlights(TypeId::of::<T>()));
        if cleared {
            cx.notify();
        }
    }

    pub fn show_local_cursors(&self, window: &mut Window, cx: &mut App) -> bool {
        (self.read_only(cx) || self.blink_manager.read(cx).visible())
            && self.focus_handle.is_focused(window)
    }

    pub fn set_show_cursor_when_unfocused(&mut self, is_enabled: bool, cx: &mut Context<Self>) {
        self.show_cursor_when_unfocused = is_enabled;
        cx.notify();
    }

    pub fn lsp_store(&self, cx: &App) -> Option<Entity<LspStore>> {
        self.project
            .as_ref()
            .map(|project| project.read(cx).lsp_store())
    }

    fn on_buffer_changed(&mut self, _: Entity<MultiBuffer>, cx: &mut Context<Self>) {
        cx.notify();
    }

    fn on_buffer_event(
        &mut self,
        multibuffer: &Entity<MultiBuffer>,
        event: &multi_buffer::Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            multi_buffer::Event::Edited {
                singleton_buffer_edited,
                edited_buffer: buffer_edited,
            } => {
                self.scrollbar_marker_state.dirty = true;
                self.active_indent_guides_state.dirty = true;
                self.refresh_active_diagnostics(cx);
                self.refresh_code_actions(window, cx);
                if self.has_active_inline_completion() {
                    self.update_visible_inline_completion(window, cx);
                }
                if let Some(buffer) = buffer_edited {
                    let buffer_id = buffer.read(cx).remote_id();
                    if !self.registered_buffers.contains_key(&buffer_id) {
                        if let Some(lsp_store) = self.lsp_store(cx) {
                            lsp_store.update(cx, |lsp_store, cx| {
                                self.registered_buffers.insert(
                                    buffer_id,
                                    lsp_store.register_buffer_with_language_servers(&buffer, cx),
                                );
                            })
                        }
                    }
                }
                cx.emit(EditorEvent::BufferEdited);
                cx.emit(SearchEvent::MatchesInvalidated);
                if *singleton_buffer_edited {
                    if let Some(project) = &self.project {
                        let project = project.read(cx);
                        #[allow(clippy::mutable_key_type)]
                        let languages_affected = multibuffer
                            .read(cx)
                            .all_buffers()
                            .into_iter()
                            .filter_map(|buffer| {
                                let buffer = buffer.read(cx);
                                let language = buffer.language()?;
                                if project.is_local()
                                    && project
                                        .language_servers_for_local_buffer(buffer, cx)
                                        .count()
                                        == 0
                                {
                                    None
                                } else {
                                    Some(language)
                                }
                            })
                            .cloned()
                            .collect::<HashSet<_>>();
                        if !languages_affected.is_empty() {
                            self.refresh_inlay_hints(
                                InlayHintRefreshReason::BufferEdited(languages_affected),
                                cx,
                            );
                        }
                    }
                }

                let Some(project) = &self.project else { return };
                let (telemetry, is_via_ssh) = {
                    let project = project.read(cx);
                    let telemetry = project.client().telemetry().clone();
                    let is_via_ssh = project.is_via_ssh();
                    (telemetry, is_via_ssh)
                };
                refresh_linked_ranges(self, window, cx);
                telemetry.log_edit_event("editor", is_via_ssh);
            }
            multi_buffer::Event::ExcerptsAdded {
                buffer,
                predecessor,
                excerpts,
            } => {
                self.tasks_update_task = Some(self.refresh_runnables(window, cx));
                let buffer_id = buffer.read(cx).remote_id();
                if self.buffer.read(cx).diff_for(buffer_id).is_none() {
                    if let Some(project) = &self.project {
                        get_uncommitted_diff_for_buffer(
                            project,
                            [buffer.clone()],
                            self.buffer.clone(),
                            cx,
                        );
                    }
                }
                cx.emit(EditorEvent::ExcerptsAdded {
                    buffer: buffer.clone(),
                    predecessor: *predecessor,
                    excerpts: excerpts.clone(),
                });
                self.refresh_inlay_hints(InlayHintRefreshReason::NewLinesShown, cx);
            }
            multi_buffer::Event::ExcerptsRemoved { ids } => {
                self.refresh_inlay_hints(InlayHintRefreshReason::ExcerptsRemoved(ids.clone()), cx);
                let buffer = self.buffer.read(cx);
                self.registered_buffers
                    .retain(|buffer_id, _| buffer.buffer(*buffer_id).is_some());
                cx.emit(EditorEvent::ExcerptsRemoved { ids: ids.clone() })
            }
            multi_buffer::Event::ExcerptsEdited { ids } => {
                cx.emit(EditorEvent::ExcerptsEdited { ids: ids.clone() })
            }
            multi_buffer::Event::ExcerptsExpanded { ids } => {
                self.refresh_inlay_hints(InlayHintRefreshReason::NewLinesShown, cx);
                cx.emit(EditorEvent::ExcerptsExpanded { ids: ids.clone() })
            }
            multi_buffer::Event::Reparsed(buffer_id) => {
                self.tasks_update_task = Some(self.refresh_runnables(window, cx));

                cx.emit(EditorEvent::Reparsed(*buffer_id));
            }
            multi_buffer::Event::DiffHunksToggled => {
                self.tasks_update_task = Some(self.refresh_runnables(window, cx));
            }
            multi_buffer::Event::LanguageChanged(buffer_id) => {
                linked_editing_ranges::refresh_linked_ranges(self, window, cx);
                cx.emit(EditorEvent::Reparsed(*buffer_id));
                cx.notify();
            }
            multi_buffer::Event::DirtyChanged => cx.emit(EditorEvent::DirtyChanged),
            multi_buffer::Event::Saved => cx.emit(EditorEvent::Saved),
            multi_buffer::Event::FileHandleChanged | multi_buffer::Event::Reloaded => {
                cx.emit(EditorEvent::TitleChanged)
            }
            // multi_buffer::Event::DiffBaseChanged => {
            //     self.scrollbar_marker_state.dirty = true;
            //     cx.emit(EditorEvent::DiffBaseChanged);
            //     cx.notify();
            // }
            multi_buffer::Event::Closed => cx.emit(EditorEvent::Closed),
            multi_buffer::Event::DiagnosticsUpdated => {
                self.refresh_active_diagnostics(cx);
                self.scrollbar_marker_state.dirty = true;
                cx.notify();
            }
            _ => {}
        };
    }

    fn on_display_map_changed(
        &mut self,
        _: Entity<DisplayMap>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.notify();
    }

    fn settings_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tasks_update_task = Some(self.refresh_runnables(window, cx));
        self.refresh_inline_completion(true, false, window, cx);
        self.refresh_inlay_hints(
            InlayHintRefreshReason::SettingsChange(inlay_hint_settings(
                self.selections.newest_anchor().head(),
                &self.buffer.read(cx).snapshot(cx),
                cx,
            )),
            cx,
        );

        let old_cursor_shape = self.cursor_shape;

        {
            let editor_settings = EditorSettings::get_global(cx);
            self.scroll_manager.vertical_scroll_margin = editor_settings.vertical_scroll_margin;
            self.show_breadcrumbs = editor_settings.toolbar.breadcrumbs;
            self.cursor_shape = editor_settings.cursor_shape.unwrap_or_default();
        }

        if old_cursor_shape != self.cursor_shape {
            cx.emit(EditorEvent::CursorShapeChanged);
        }

        let project_settings = ProjectSettings::get_global(cx);
        self.serialize_dirty_buffers = project_settings.session.restore_unsaved_buffers;

        if self.mode == EditorMode::Full {
            let inline_blame_enabled = project_settings.git.inline_blame_enabled();
            if self.git_blame_inline_enabled != inline_blame_enabled {
                self.toggle_git_blame_inline_internal(false, window, cx);
            }
        }

        cx.notify();
    }

    pub fn set_searchable(&mut self, searchable: bool) {
        self.searchable = searchable;
    }

    pub fn searchable(&self) -> bool {
        self.searchable
    }

    fn open_proposed_changes_editor(
        &mut self,
        _: &OpenProposedChangesEditor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace() else {
            cx.propagate();
            return;
        };

        let selections = self.selections.all::<usize>(cx);
        let multi_buffer = self.buffer.read(cx);
        let multi_buffer_snapshot = multi_buffer.snapshot(cx);
        let mut new_selections_by_buffer = HashMap::default();
        for selection in selections {
            for (buffer, range, _) in
                multi_buffer_snapshot.range_to_buffer_ranges(selection.start..selection.end)
            {
                let mut range = range.to_point(buffer);
                range.start.column = 0;
                range.end.column = buffer.line_len(range.end.row);
                new_selections_by_buffer
                    .entry(multi_buffer.buffer(buffer.remote_id()).unwrap())
                    .or_insert(Vec::new())
                    .push(range)
            }
        }

        let proposed_changes_buffers = new_selections_by_buffer
            .into_iter()
            .map(|(buffer, ranges)| ProposedChangeLocation { buffer, ranges })
            .collect::<Vec<_>>();
        let proposed_changes_editor = cx.new(|cx| {
            ProposedChangesEditor::new(
                "Proposed changes",
                proposed_changes_buffers,
                self.project.clone(),
                window,
                cx,
            )
        });

        window.defer(cx, move |window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.active_pane().update(cx, |pane, cx| {
                    pane.add_item(
                        Box::new(proposed_changes_editor),
                        true,
                        true,
                        None,
                        window,
                        cx,
                    );
                });
            });
        });
    }

    pub fn open_excerpts_in_split(
        &mut self,
        _: &OpenExcerptsSplit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_excerpts_common(None, true, window, cx)
    }

    pub fn open_excerpts(&mut self, _: &OpenExcerpts, window: &mut Window, cx: &mut Context<Self>) {
        self.open_excerpts_common(None, false, window, cx)
    }

    fn open_excerpts_common(
        &mut self,
        jump_data: Option<JumpData>,
        split: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace() else {
            cx.propagate();
            return;
        };

        if self.buffer.read(cx).is_singleton() {
            cx.propagate();
            return;
        }

        let mut new_selections_by_buffer = HashMap::default();
        match &jump_data {
            Some(JumpData::MultiBufferPoint {
                excerpt_id,
                position,
                anchor,
                line_offset_from_top,
            }) => {
                let multi_buffer_snapshot = self.buffer.read(cx).snapshot(cx);
                if let Some(buffer) = multi_buffer_snapshot
                    .buffer_id_for_excerpt(*excerpt_id)
                    .and_then(|buffer_id| self.buffer.read(cx).buffer(buffer_id))
                {
                    let buffer_snapshot = buffer.read(cx).snapshot();
                    let jump_to_point = if buffer_snapshot.can_resolve(anchor) {
                        language::ToPoint::to_point(anchor, &buffer_snapshot)
                    } else {
                        buffer_snapshot.clip_point(*position, Bias::Left)
                    };
                    let jump_to_offset = buffer_snapshot.point_to_offset(jump_to_point);
                    new_selections_by_buffer.insert(
                        buffer,
                        (
                            vec![jump_to_offset..jump_to_offset],
                            Some(*line_offset_from_top),
                        ),
                    );
                }
            }
            Some(JumpData::MultiBufferRow {
                row,
                line_offset_from_top,
            }) => {
                let point = MultiBufferPoint::new(row.0, 0);
                if let Some((buffer, buffer_point, _)) =
                    self.buffer.read(cx).point_to_buffer_point(point, cx)
                {
                    let buffer_offset = buffer.read(cx).point_to_offset(buffer_point);
                    new_selections_by_buffer
                        .entry(buffer)
                        .or_insert((Vec::new(), Some(*line_offset_from_top)))
                        .0
                        .push(buffer_offset..buffer_offset)
                }
            }
            None => {
                let selections = self.selections.all::<usize>(cx);
                let multi_buffer = self.buffer.read(cx);
                for selection in selections {
                    for (buffer, mut range, _) in multi_buffer
                        .snapshot(cx)
                        .range_to_buffer_ranges(selection.range())
                    {
                        // When editing branch buffers, jump to the corresponding location
                        // in their base buffer.
                        let mut buffer_handle = multi_buffer.buffer(buffer.remote_id()).unwrap();
                        let buffer = buffer_handle.read(cx);
                        if let Some(base_buffer) = buffer.base_buffer() {
                            range = buffer.range_to_version(range, &base_buffer.read(cx).version());
                            buffer_handle = base_buffer;
                        }

                        if selection.reversed {
                            mem::swap(&mut range.start, &mut range.end);
                        }
                        new_selections_by_buffer
                            .entry(buffer_handle)
                            .or_insert((Vec::new(), None))
                            .0
                            .push(range)
                    }
                }
            }
        }

        if new_selections_by_buffer.is_empty() {
            return;
        }

        // We defer the pane interaction because we ourselves are a workspace item
        // and activating a new item causes the pane to call a method on us reentrantly,
        // which panics if we're on the stack.
        window.defer(cx, move |window, cx| {
            workspace.update(cx, |workspace, cx| {
                let pane = if split {
                    workspace.adjacent_pane(window, cx)
                } else {
                    workspace.active_pane().clone()
                };

                for (buffer, (ranges, scroll_offset)) in new_selections_by_buffer {
                    let editor = buffer
                        .read(cx)
                        .file()
                        .is_none()
                        .then(|| {
                            // Handle file-less buffers separately: those are not really the project items, so won't have a project path or entity id,
                            // so `workspace.open_project_item` will never find them, always opening a new editor.
                            // Instead, we try to activate the existing editor in the pane first.
                            let (editor, pane_item_index) =
                                pane.read(cx).items().enumerate().find_map(|(i, item)| {
                                    let editor = item.downcast::<Editor>()?;
                                    let singleton_buffer =
                                        editor.read(cx).buffer().read(cx).as_singleton()?;
                                    if singleton_buffer == buffer {
                                        Some((editor, i))
                                    } else {
                                        None
                                    }
                                })?;
                            pane.update(cx, |pane, cx| {
                                pane.activate_item(pane_item_index, true, true, window, cx)
                            });
                            Some(editor)
                        })
                        .flatten()
                        .unwrap_or_else(|| {
                            workspace.open_project_item::<Self>(
                                pane.clone(),
                                buffer,
                                true,
                                true,
                                window,
                                cx,
                            )
                        });

                    editor.update(cx, |editor, cx| {
                        let autoscroll = match scroll_offset {
                            Some(scroll_offset) => Autoscroll::top_relative(scroll_offset as usize),
                            None => Autoscroll::newest(),
                        };
                        let nav_history = editor.nav_history.take();
                        editor.change_selections(Some(autoscroll), window, cx, |s| {
                            s.select_ranges(ranges);
                        });
                        editor.nav_history = nav_history;
                    });
                }
            })
        });
    }

    fn marked_text_ranges(&self, cx: &App) -> Option<Vec<Range<OffsetUtf16>>> {
        let snapshot = self.buffer.read(cx).read(cx);
        let (_, ranges) = self.text_highlights::<InputComposition>(cx)?;
        Some(
            ranges
                .iter()
                .map(move |range| {
                    range.start.to_offset_utf16(&snapshot)..range.end.to_offset_utf16(&snapshot)
                })
                .collect(),
        )
    }

    fn selection_replacement_ranges(
        &self,
        range: Range<OffsetUtf16>,
        cx: &mut App,
    ) -> Vec<Range<OffsetUtf16>> {
        let selections = self.selections.all::<OffsetUtf16>(cx);
        let newest_selection = selections
            .iter()
            .max_by_key(|selection| selection.id)
            .unwrap();
        let start_delta = range.start.0 as isize - newest_selection.start.0 as isize;
        let end_delta = range.end.0 as isize - newest_selection.end.0 as isize;
        let snapshot = self.buffer.read(cx).read(cx);
        selections
            .into_iter()
            .map(|mut selection| {
                selection.start.0 =
                    (selection.start.0 as isize).saturating_add(start_delta) as usize;
                selection.end.0 = (selection.end.0 as isize).saturating_add(end_delta) as usize;
                snapshot.clip_offset_utf16(selection.start, Bias::Left)
                    ..snapshot.clip_offset_utf16(selection.end, Bias::Right)
            })
            .collect()
    }

    fn report_editor_event(
        &self,
        event_type: &'static str,
        file_extension: Option<String>,
        cx: &App,
    ) {
        if cfg!(any(test, feature = "test-support")) {
            return;
        }

        let Some(project) = &self.project else { return };

        // If None, we are in a file without an extension
        let file = self
            .buffer
            .read(cx)
            .as_singleton()
            .and_then(|b| b.read(cx).file());
        let file_extension = file_extension.or(file
            .as_ref()
            .and_then(|file| Path::new(file.file_name(cx)).extension())
            .and_then(|e| e.to_str())
            .map(|a| a.to_string()));

        let vim_mode = cx
            .global::<SettingsStore>()
            .raw_user_settings()
            .get("vim_mode")
            == Some(&serde_json::Value::Bool(true));

        let edit_predictions_provider = all_language_settings(file, cx).inline_completions.provider;
        let copilot_enabled = edit_predictions_provider
            == language::language_settings::InlineCompletionProvider::Copilot;
        let copilot_enabled_for_language = self
            .buffer
            .read(cx)
            .settings_at(0, cx)
            .show_inline_completions;

        let project = project.read(cx);
        telemetry::event!(
            event_type,
            file_extension,
            vim_mode,
            copilot_enabled,
            copilot_enabled_for_language,
            edit_predictions_provider,
            is_via_ssh = project.is_via_ssh(),
        );
    }

    /// Copy the highlighted chunks to the clipboard as JSON. The format is an array of lines,
    /// with each line being an array of {text, highlight} objects.
    fn copy_highlight_json(
        &mut self,
        _: &CopyHighlightJson,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[derive(Serialize)]
        struct Chunk<'a> {
            text: String,
            highlight: Option<&'a str>,
        }

        let snapshot = self.buffer.read(cx).snapshot(cx);
        let range = self
            .selected_text_range(false, window, cx)
            .and_then(|selection| {
                if selection.range.is_empty() {
                    None
                } else {
                    Some(selection.range)
                }
            })
            .unwrap_or_else(|| 0..snapshot.len());

        let chunks = snapshot.chunks(range, true);
        let mut lines = Vec::new();
        let mut line: VecDeque<Chunk> = VecDeque::new();

        let Some(style) = self.style.as_ref() else {
            return;
        };

        for chunk in chunks {
            let highlight = chunk
                .syntax_highlight_id
                .and_then(|id| id.name(&style.syntax));
            let mut chunk_lines = chunk.text.split('\n').peekable();
            while let Some(text) = chunk_lines.next() {
                let mut merged_with_last_token = false;
                if let Some(last_token) = line.back_mut() {
                    if last_token.highlight == highlight {
                        last_token.text.push_str(text);
                        merged_with_last_token = true;
                    }
                }

                if !merged_with_last_token {
                    line.push_back(Chunk {
                        text: text.into(),
                        highlight,
                    });
                }

                if chunk_lines.peek().is_some() {
                    if line.len() > 1 && line.front().unwrap().text.is_empty() {
                        line.pop_front();
                    }
                    if line.len() > 1 && line.back().unwrap().text.is_empty() {
                        line.pop_back();
                    }

                    lines.push(mem::take(&mut line));
                }
            }
        }

        let Some(lines) = serde_json::to_string_pretty(&lines).log_err() else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(lines));
    }

    pub fn open_context_menu(
        &mut self,
        _: &OpenContextMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_autoscroll(Autoscroll::newest(), cx);
        let position = self.selections.newest_display(cx).start;
        mouse_context_menu::deploy_context_menu(self, None, position, window, cx);
    }

    pub fn inlay_hint_cache(&self) -> &InlayHintCache {
        &self.inlay_hint_cache
    }

    pub fn replay_insert_event(
        &mut self,
        text: &str,
        relative_utf16_range: Option<Range<isize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_enabled {
            cx.emit(EditorEvent::InputIgnored { text: text.into() });
            return;
        }
        if let Some(relative_utf16_range) = relative_utf16_range {
            let selections = self.selections.all::<OffsetUtf16>(cx);
            self.change_selections(None, window, cx, |s| {
                let new_ranges = selections.into_iter().map(|range| {
                    let start = OffsetUtf16(
                        range
                            .head()
                            .0
                            .saturating_add_signed(relative_utf16_range.start),
                    );
                    let end = OffsetUtf16(
                        range
                            .head()
                            .0
                            .saturating_add_signed(relative_utf16_range.end),
                    );
                    start..end
                });
                s.select_ranges(new_ranges);
            });
        }

        self.handle_input(text, window, cx);
    }

    pub fn supports_inlay_hints(&self, cx: &App) -> bool {
        let Some(provider) = self.semantics_provider.as_ref() else {
            return false;
        };

        let mut supports = false;
        self.buffer().read(cx).for_each_buffer(|buffer| {
            supports |= provider.supports_inlay_hints(buffer, cx);
        });
        supports
    }
    pub fn is_focused(&self, window: &mut Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    fn handle_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(EditorEvent::Focused);

        if let Some(descendant) = self
            .last_focused_descendant
            .take()
            .and_then(|descendant| descendant.upgrade())
        {
            window.focus(&descendant);
        } else {
            if let Some(blame) = self.blame.as_ref() {
                blame.update(cx, GitBlame::focus)
            }

            self.blink_manager.update(cx, BlinkManager::enable);
            self.show_cursor_names(window, cx);
            self.buffer.update(cx, |buffer, cx| {
                buffer.finalize_last_transaction(cx);
                if self.leader_peer_id.is_none() {
                    buffer.set_active_selections(
                        &self.selections.disjoint_anchors(),
                        self.selections.line_mode,
                        self.cursor_shape,
                        cx,
                    );
                }
            });
        }
    }

    fn handle_focus_in(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(EditorEvent::FocusedIn)
    }

    fn handle_focus_out(
        &mut self,
        event: FocusOutEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        if event.blurred != self.focus_handle {
            self.last_focused_descendant = Some(event.blurred);
        }
    }

    pub fn handle_blur(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.blink_manager.update(cx, BlinkManager::disable);
        self.buffer
            .update(cx, |buffer, cx| buffer.remove_active_selections(cx));

        if let Some(blame) = self.blame.as_ref() {
            blame.update(cx, GitBlame::blur)
        }
        if !self.hover_state.focused(window, cx) {
            hide_hover(self, cx);
        }

        self.hide_context_menu(window, cx);
        cx.emit(EditorEvent::Blurred);
        cx.notify();
    }

    pub fn register_action<A: Action>(
        &mut self,
        listener: impl Fn(&A, &mut Window, &mut App) + 'static,
    ) -> Subscription {
        let id = self.next_editor_action_id.post_inc();
        let listener = Arc::new(listener);
        self.editor_actions.borrow_mut().insert(
            id,
            Box::new(move |window, _| {
                let listener = listener.clone();
                window.on_action(TypeId::of::<A>(), move |action, phase, window, cx| {
                    let action = action.downcast_ref().unwrap();
                    if phase == DispatchPhase::Bubble {
                        listener(action, window, cx)
                    }
                })
            }),
        );

        let editor_actions = self.editor_actions.clone();
        Subscription::new(move || {
            editor_actions.borrow_mut().remove(&id);
        })
    }

    pub fn file_header_size(&self) -> u32 {
        FILE_HEADER_HEIGHT
    }

    pub fn revert(
        &mut self,
        revert_changes: HashMap<BufferId, Vec<(Range<text::Anchor>, Rope)>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer().update(cx, |multi_buffer, cx| {
            for (buffer_id, changes) in revert_changes {
                if let Some(buffer) = multi_buffer.buffer(buffer_id) {
                    buffer.update(cx, |buffer, cx| {
                        buffer.edit(
                            changes.into_iter().map(|(range, text)| {
                                (range, text.to_string().map(Arc::<str>::from))
                            }),
                            None,
                            cx,
                        );
                    });
                }
            }
        });
        self.change_selections(None, window, cx, |selections| selections.refresh());
    }

    pub fn to_pixel_point(
        &self,
        source: multi_buffer::Anchor,
        editor_snapshot: &EditorSnapshot,
        window: &mut Window,
    ) -> Option<gpui::Point<Pixels>> {
        let source_point = source.to_display_point(editor_snapshot);
        self.display_to_pixel_point(source_point, editor_snapshot, window)
    }

    pub fn display_to_pixel_point(
        &self,
        source: DisplayPoint,
        editor_snapshot: &EditorSnapshot,
        window: &mut Window,
    ) -> Option<gpui::Point<Pixels>> {
        let line_height = self.style()?.text.line_height_in_pixels(window.rem_size());
        let text_layout_details = self.text_layout_details(window);
        let scroll_top = text_layout_details
            .scroll_anchor
            .scroll_position(editor_snapshot)
            .y;

        if source.row().as_f32() < scroll_top.floor() {
            return None;
        }
        let source_x = editor_snapshot.x_for_display_point(source, &text_layout_details);
        let source_y = line_height * (source.row().as_f32() - scroll_top);
        Some(gpui::Point::new(source_x, source_y))
    }

    pub fn has_visible_completions_menu(&self) -> bool {
        !self.previewing_inline_completion
            && self.context_menu.borrow().as_ref().map_or(false, |menu| {
                menu.visible() && matches!(menu, CodeContextMenu::Completions(_))
            })
    }

    pub fn register_addon<T: Addon>(&mut self, instance: T) {
        self.addons
            .insert(std::any::TypeId::of::<T>(), Box::new(instance));
    }

    pub fn unregister_addon<T: Addon>(&mut self) {
        self.addons.remove(&std::any::TypeId::of::<T>());
    }

    pub fn addon<T: Addon>(&self) -> Option<&T> {
        let type_id = std::any::TypeId::of::<T>();
        self.addons
            .get(&type_id)
            .and_then(|item| item.to_any().downcast_ref::<T>())
    }

    fn character_size(&self, window: &mut Window) -> gpui::Size<Pixels> {
        let text_layout_details = self.text_layout_details(window);
        let style = &text_layout_details.editor_style;
        let font_id = window.text_system().resolve_font(&style.text.font());
        let font_size = style.text.font_size.to_pixels(window.rem_size());
        let line_height = style.text.line_height_in_pixels(window.rem_size());
        let em_width = window.text_system().em_width(font_id, font_size).unwrap();

        gpui::Size::new(em_width, line_height)
    }
}

fn get_uncommitted_diff_for_buffer(
    project: &Entity<Project>,
    buffers: impl IntoIterator<Item = Entity<Buffer>>,
    buffer: Entity<MultiBuffer>,
    cx: &mut App,
) {
    let mut tasks = Vec::new();
    project.update(cx, |project, cx| {
        for buffer in buffers {
            tasks.push(project.open_uncommitted_diff(buffer.clone(), cx))
        }
    });
    cx.spawn(|mut cx| async move {
        let diffs = futures::future::join_all(tasks).await;
        buffer
            .update(&mut cx, |buffer, cx| {
                for diff in diffs.into_iter().flatten() {
                    buffer.add_diff(diff, cx);
                }
            })
            .ok();
    })
    .detach();
}

fn char_len_with_expanded_tabs(offset: usize, text: &str, tab_size: NonZeroU32) -> usize {
    let tab_size = tab_size.get() as usize;
    let mut width = offset;

    for ch in text.chars() {
        width += if ch == '\t' {
            tab_size - (width % tab_size)
        } else {
            1
        };
    }

    width - offset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_size_with_expanded_tabs() {
        let nz = |val| NonZeroU32::new(val).unwrap();
        assert_eq!(char_len_with_expanded_tabs(0, "", nz(4)), 0);
        assert_eq!(char_len_with_expanded_tabs(0, "hello", nz(4)), 5);
        assert_eq!(char_len_with_expanded_tabs(0, "\thello", nz(4)), 9);
        assert_eq!(char_len_with_expanded_tabs(0, "abc\tab", nz(4)), 6);
        assert_eq!(char_len_with_expanded_tabs(0, "hello\t", nz(4)), 8);
        assert_eq!(char_len_with_expanded_tabs(0, "\t\t", nz(8)), 16);
        assert_eq!(char_len_with_expanded_tabs(0, "x\t", nz(8)), 8);
        assert_eq!(char_len_with_expanded_tabs(7, "x\t", nz(8)), 9);
    }
}

/// Tokenizes a string into runs of text that should stick together, or that is whitespace.
struct WordBreakingTokenizer<'a> {
    input: &'a str,
}

impl<'a> WordBreakingTokenizer<'a> {
    fn new(input: &'a str) -> Self {
        Self { input }
    }
}

fn is_char_ideographic(ch: char) -> bool {
    use unicode_script::Script::*;
    use unicode_script::UnicodeScript;
    matches!(ch.script(), Han | Tangut | Yi)
}

fn is_grapheme_ideographic(text: &str) -> bool {
    text.chars().any(is_char_ideographic)
}

fn is_grapheme_whitespace(text: &str) -> bool {
    text.chars().any(|x| x.is_whitespace())
}

fn should_stay_with_preceding_ideograph(text: &str) -> bool {
    text.chars().next().map_or(false, |ch| {
        matches!(ch, '。' | '、' | '，' | '？' | '！' | '：' | '；' | '…')
    })
}

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
struct WordBreakToken<'a> {
    token: &'a str,
    grapheme_len: usize,
    is_whitespace: bool,
}

impl<'a> Iterator for WordBreakingTokenizer<'a> {
    /// Yields a span, the count of graphemes in the token, and whether it was
    /// whitespace. Note that it also breaks at word boundaries.
    type Item = WordBreakToken<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        use unicode_segmentation::UnicodeSegmentation;
        if self.input.is_empty() {
            return None;
        }

        let mut iter = self.input.graphemes(true).peekable();
        let mut offset = 0;
        let mut graphemes = 0;
        if let Some(first_grapheme) = iter.next() {
            let is_whitespace = is_grapheme_whitespace(first_grapheme);
            offset += first_grapheme.len();
            graphemes += 1;
            if is_grapheme_ideographic(first_grapheme) && !is_whitespace {
                if let Some(grapheme) = iter.peek().copied() {
                    if should_stay_with_preceding_ideograph(grapheme) {
                        offset += grapheme.len();
                        graphemes += 1;
                    }
                }
            } else {
                let mut words = self.input[offset..].split_word_bound_indices().peekable();
                let mut next_word_bound = words.peek().copied();
                if next_word_bound.map_or(false, |(i, _)| i == 0) {
                    next_word_bound = words.next();
                }
                while let Some(grapheme) = iter.peek().copied() {
                    if next_word_bound.map_or(false, |(i, _)| i == offset) {
                        break;
                    };
                    if is_grapheme_whitespace(grapheme) != is_whitespace {
                        break;
                    };
                    offset += grapheme.len();
                    graphemes += 1;
                    iter.next();
                }
            }
            let token = &self.input[..offset];
            self.input = &self.input[offset..];
            if is_whitespace {
                Some(WordBreakToken {
                    token: " ",
                    grapheme_len: 1,
                    is_whitespace: true,
                })
            } else {
                Some(WordBreakToken {
                    token,
                    grapheme_len: graphemes,
                    is_whitespace: false,
                })
            }
        } else {
            None
        }
    }
}

#[test]
fn test_word_breaking_tokenizer() {
    let tests: &[(&str, &[(&str, usize, bool)])] = &[
        ("", &[]),
        ("  ", &[(" ", 1, true)]),
        ("Ʒ", &[("Ʒ", 1, false)]),
        ("Ǽ", &[("Ǽ", 1, false)]),
        ("⋑", &[("⋑", 1, false)]),
        ("⋑⋑", &[("⋑⋑", 2, false)]),
        (
            "原理，进而",
            &[
                ("原", 1, false),
                ("理，", 2, false),
                ("进", 1, false),
                ("而", 1, false),
            ],
        ),
        (
            "hello world",
            &[("hello", 5, false), (" ", 1, true), ("world", 5, false)],
        ),
        (
            "hello, world",
            &[("hello,", 6, false), (" ", 1, true), ("world", 5, false)],
        ),
        (
            "  hello world",
            &[
                (" ", 1, true),
                ("hello", 5, false),
                (" ", 1, true),
                ("world", 5, false),
            ],
        ),
        (
            "这是什么 \n 钢笔",
            &[
                ("这", 1, false),
                ("是", 1, false),
                ("什", 1, false),
                ("么", 1, false),
                (" ", 1, true),
                ("钢", 1, false),
                ("笔", 1, false),
            ],
        ),
        (" mutton", &[(" ", 1, true), ("mutton", 6, false)]),
    ];

    for (input, result) in tests {
        assert_eq!(
            WordBreakingTokenizer::new(input).collect::<Vec<_>>(),
            result
                .iter()
                .copied()
                .map(|(token, grapheme_len, is_whitespace)| WordBreakToken {
                    token,
                    grapheme_len,
                    is_whitespace,
                })
                .collect::<Vec<_>>()
        );
    }
}

fn wrap_with_prefix(
    line_prefix: String,
    unwrapped_text: String,
    wrap_column: usize,
    tab_size: NonZeroU32,
) -> String {
    let line_prefix_len = char_len_with_expanded_tabs(0, &line_prefix, tab_size);
    let mut wrapped_text = String::new();
    let mut current_line = line_prefix.clone();

    let tokenizer = WordBreakingTokenizer::new(&unwrapped_text);
    let mut current_line_len = line_prefix_len;
    for WordBreakToken {
        token,
        grapheme_len,
        is_whitespace,
    } in tokenizer
    {
        if current_line_len + grapheme_len > wrap_column && current_line_len != line_prefix_len {
            wrapped_text.push_str(current_line.trim_end());
            wrapped_text.push('\n');
            current_line.truncate(line_prefix.len());
            current_line_len = line_prefix_len;
            if !is_whitespace {
                current_line.push_str(token);
                current_line_len += grapheme_len;
            }
        } else if !is_whitespace {
            current_line.push_str(token);
            current_line_len += grapheme_len;
        } else if current_line_len != line_prefix_len {
            current_line.push(' ');
            current_line_len += 1;
        }
    }

    if !current_line.is_empty() {
        wrapped_text.push_str(&current_line);
    }
    wrapped_text
}

#[test]
fn test_wrap_with_prefix() {
    assert_eq!(
        wrap_with_prefix(
            "# ".to_string(),
            "abcdefg".to_string(),
            4,
            NonZeroU32::new(4).unwrap()
        ),
        "# abcdefg"
    );
    assert_eq!(
        wrap_with_prefix(
            "".to_string(),
            "\thello world".to_string(),
            8,
            NonZeroU32::new(4).unwrap()
        ),
        "hello\nworld"
    );
    assert_eq!(
        wrap_with_prefix(
            "// ".to_string(),
            "xx \nyy zz aa bb cc".to_string(),
            12,
            NonZeroU32::new(4).unwrap()
        ),
        "// xx yy zz\n// aa bb cc"
    );
    assert_eq!(
        wrap_with_prefix(
            String::new(),
            "这是什么 \n 钢笔".to_string(),
            3,
            NonZeroU32::new(4).unwrap()
        ),
        "这是什\n么 钢\n笔"
    );
}

pub trait CollaborationHub {
    fn collaborators<'a>(&self, cx: &'a App) -> &'a HashMap<PeerId, Collaborator>;
    fn user_participant_indices<'a>(&self, cx: &'a App) -> &'a HashMap<u64, ParticipantIndex>;
    fn user_names(&self, cx: &App) -> HashMap<u64, SharedString>;
}

impl CollaborationHub for Entity<Project> {
    fn collaborators<'a>(&self, cx: &'a App) -> &'a HashMap<PeerId, Collaborator> {
        self.read(cx).collaborators()
    }

    fn user_participant_indices<'a>(&self, cx: &'a App) -> &'a HashMap<u64, ParticipantIndex> {
        self.read(cx).user_store().read(cx).participant_indices()
    }

    fn user_names(&self, cx: &App) -> HashMap<u64, SharedString> {
        let this = self.read(cx);
        let user_ids = this.collaborators().values().map(|c| c.user_id);
        this.user_store().read_with(cx, |user_store, cx| {
            user_store.participant_names(user_ids, cx)
        })
    }
}

pub trait SemanticsProvider {
    fn hover(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        cx: &mut App,
    ) -> Option<Task<Vec<project::Hover>>>;

    fn inlay_hints(
        &self,
        buffer_handle: Entity<Buffer>,
        range: Range<text::Anchor>,
        cx: &mut App,
    ) -> Option<Task<anyhow::Result<Vec<InlayHint>>>>;

    fn resolve_inlay_hint(
        &self,
        hint: InlayHint,
        buffer_handle: Entity<Buffer>,
        server_id: LanguageServerId,
        cx: &mut App,
    ) -> Option<Task<anyhow::Result<InlayHint>>>;

    fn supports_inlay_hints(&self, buffer: &Entity<Buffer>, cx: &App) -> bool;

    fn document_highlights(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        cx: &mut App,
    ) -> Option<Task<Result<Vec<DocumentHighlight>>>>;

    fn definitions(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        kind: GotoDefinitionKind,
        cx: &mut App,
    ) -> Option<Task<Result<Vec<LocationLink>>>>;

    fn range_for_rename(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        cx: &mut App,
    ) -> Option<Task<Result<Option<Range<text::Anchor>>>>>;

    fn perform_rename(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        new_name: String,
        cx: &mut App,
    ) -> Option<Task<Result<ProjectTransaction>>>;
}

pub trait CompletionProvider {
    fn completions(
        &self,
        buffer: &Entity<Buffer>,
        buffer_position: text::Anchor,
        trigger: CompletionContext,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) -> Task<Result<Vec<Completion>>>;

    fn resolve_completions(
        &self,
        buffer: Entity<Buffer>,
        completion_indices: Vec<usize>,
        completions: Rc<RefCell<Box<[Completion]>>>,
        cx: &mut Context<Editor>,
    ) -> Task<Result<bool>>;

    fn apply_additional_edits_for_completion(
        &self,
        _buffer: Entity<Buffer>,
        _completions: Rc<RefCell<Box<[Completion]>>>,
        _completion_index: usize,
        _push_to_history: bool,
        _cx: &mut Context<Editor>,
    ) -> Task<Result<Option<language::Transaction>>> {
        Task::ready(Ok(None))
    }

    fn is_completion_trigger(
        &self,
        buffer: &Entity<Buffer>,
        position: language::Anchor,
        text: &str,
        trigger_in_words: bool,
        cx: &mut Context<Editor>,
    ) -> bool;

    fn sort_completions(&self) -> bool {
        true
    }
}

pub trait CodeActionProvider {
    fn id(&self) -> Arc<str>;

    fn code_actions(
        &self,
        buffer: &Entity<Buffer>,
        range: Range<text::Anchor>,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<CodeAction>>>;

    fn apply_code_action(
        &self,
        buffer_handle: Entity<Buffer>,
        action: CodeAction,
        excerpt_id: ExcerptId,
        push_to_history: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<ProjectTransaction>>;
}

impl CodeActionProvider for Entity<Project> {
    fn id(&self) -> Arc<str> {
        "project".into()
    }

    fn code_actions(
        &self,
        buffer: &Entity<Buffer>,
        range: Range<text::Anchor>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<CodeAction>>> {
        self.update(cx, |project, cx| {
            project.code_actions(buffer, range, None, cx)
        })
    }

    fn apply_code_action(
        &self,
        buffer_handle: Entity<Buffer>,
        action: CodeAction,
        _excerpt_id: ExcerptId,
        push_to_history: bool,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<ProjectTransaction>> {
        self.update(cx, |project, cx| {
            project.apply_code_action(buffer_handle, action, push_to_history, cx)
        })
    }
}

fn snippet_completions(
    project: &Project,
    buffer: &Entity<Buffer>,
    buffer_position: text::Anchor,
    cx: &mut App,
) -> Task<Result<Vec<Completion>>> {
    let language = buffer.read(cx).language_at(buffer_position);
    let language_name = language.as_ref().map(|language| language.lsp_id());
    let snippet_store = project.snippets().read(cx);
    let snippets = snippet_store.snippets_for(language_name, cx);

    if snippets.is_empty() {
        return Task::ready(Ok(vec![]));
    }
    let snapshot = buffer.read(cx).text_snapshot();
    let chars: String = snapshot
        .reversed_chars_for_range(text::Anchor::MIN..buffer_position)
        .collect();

    let scope = language.map(|language| language.default_scope());
    let executor = cx.background_executor().clone();

    cx.background_executor().spawn(async move {
        let classifier = CharClassifier::new(scope).for_completion(true);
        let mut last_word = chars
            .chars()
            .take_while(|c| classifier.is_word(*c))
            .collect::<String>();
        last_word = last_word.chars().rev().collect();

        if last_word.is_empty() {
            return Ok(vec![]);
        }

        let as_offset = text::ToOffset::to_offset(&buffer_position, &snapshot);
        let to_lsp = |point: &text::Anchor| {
            let end = text::ToPointUtf16::to_point_utf16(point, &snapshot);
            point_to_lsp(end)
        };
        let lsp_end = to_lsp(&buffer_position);

        let candidates = snippets
            .iter()
            .enumerate()
            .flat_map(|(ix, snippet)| {
                snippet
                    .prefix
                    .iter()
                    .map(move |prefix| StringMatchCandidate::new(ix, &prefix))
            })
            .collect::<Vec<StringMatchCandidate>>();

        let mut matches = fuzzy::match_strings(
            &candidates,
            &last_word,
            last_word.chars().any(|c| c.is_uppercase()),
            100,
            &Default::default(),
            executor,
        )
        .await;

        // Remove all candidates where the query's start does not match the start of any word in the candidate
        if let Some(query_start) = last_word.chars().next() {
            matches.retain(|string_match| {
                split_words(&string_match.string).any(|word| {
                    // Check that the first codepoint of the word as lowercase matches the first
                    // codepoint of the query as lowercase
                    word.chars()
                        .flat_map(|codepoint| codepoint.to_lowercase())
                        .zip(query_start.to_lowercase())
                        .all(|(word_cp, query_cp)| word_cp == query_cp)
                })
            });
        }

        let matched_strings = matches
            .into_iter()
            .map(|m| m.string)
            .collect::<HashSet<_>>();

        let result: Vec<Completion> = snippets
            .into_iter()
            .filter_map(|snippet| {
                let matching_prefix = snippet
                    .prefix
                    .iter()
                    .find(|prefix| matched_strings.contains(*prefix))?;
                let start = as_offset - last_word.len();
                let start = snapshot.anchor_before(start);
                let range = start..buffer_position;
                let lsp_start = to_lsp(&start);
                let lsp_range = lsp::Range {
                    start: lsp_start,
                    end: lsp_end,
                };
                Some(Completion {
                    old_range: range,
                    new_text: snippet.body.clone(),
                    resolved: false,
                    label: CodeLabel {
                        text: matching_prefix.clone(),
                        runs: vec![],
                        filter_range: 0..matching_prefix.len(),
                    },
                    server_id: LanguageServerId(usize::MAX),
                    documentation: snippet
                        .description
                        .clone()
                        .map(CompletionDocumentation::SingleLine),
                    lsp_completion: lsp::CompletionItem {
                        label: snippet.prefix.first().unwrap().clone(),
                        kind: Some(CompletionItemKind::SNIPPET),
                        label_details: snippet.description.as_ref().map(|description| {
                            lsp::CompletionItemLabelDetails {
                                detail: Some(description.clone()),
                                description: None,
                            }
                        }),
                        insert_text_format: Some(InsertTextFormat::SNIPPET),
                        text_edit: Some(lsp::CompletionTextEdit::InsertAndReplace(
                            lsp::InsertReplaceEdit {
                                new_text: snippet.body.clone(),
                                insert: lsp_range,
                                replace: lsp_range,
                            },
                        )),
                        filter_text: Some(snippet.body.clone()),
                        sort_text: Some(char::MAX.to_string()),
                        ..Default::default()
                    },
                    confirm: None,
                })
            })
            .collect();

        Ok(result)
    })
}

impl CompletionProvider for Entity<Project> {
    fn completions(
        &self,
        buffer: &Entity<Buffer>,
        buffer_position: text::Anchor,
        options: CompletionContext,
        _window: &mut Window,
        cx: &mut Context<Editor>,
    ) -> Task<Result<Vec<Completion>>> {
        self.update(cx, |project, cx| {
            let snippets = snippet_completions(project, buffer, buffer_position, cx);
            let project_completions = project.completions(buffer, buffer_position, options, cx);
            cx.background_executor().spawn(async move {
                let mut completions = project_completions.await?;
                let snippets_completions = snippets.await?;
                completions.extend(snippets_completions);
                Ok(completions)
            })
        })
    }

    fn resolve_completions(
        &self,
        buffer: Entity<Buffer>,
        completion_indices: Vec<usize>,
        completions: Rc<RefCell<Box<[Completion]>>>,
        cx: &mut Context<Editor>,
    ) -> Task<Result<bool>> {
        self.update(cx, |project, cx| {
            project.lsp_store().update(cx, |lsp_store, cx| {
                lsp_store.resolve_completions(buffer, completion_indices, completions, cx)
            })
        })
    }

    fn apply_additional_edits_for_completion(
        &self,
        buffer: Entity<Buffer>,
        completions: Rc<RefCell<Box<[Completion]>>>,
        completion_index: usize,
        push_to_history: bool,
        cx: &mut Context<Editor>,
    ) -> Task<Result<Option<language::Transaction>>> {
        self.update(cx, |project, cx| {
            project.lsp_store().update(cx, |lsp_store, cx| {
                lsp_store.apply_additional_edits_for_completion(
                    buffer,
                    completions,
                    completion_index,
                    push_to_history,
                    cx,
                )
            })
        })
    }

    fn is_completion_trigger(
        &self,
        buffer: &Entity<Buffer>,
        position: language::Anchor,
        text: &str,
        trigger_in_words: bool,
        cx: &mut Context<Editor>,
    ) -> bool {
        let mut chars = text.chars();
        let char = if let Some(char) = chars.next() {
            char
        } else {
            return false;
        };
        if chars.next().is_some() {
            return false;
        }

        let buffer = buffer.read(cx);
        let snapshot = buffer.snapshot();
        if !snapshot.settings_at(position, cx).show_completions_on_input {
            return false;
        }
        let classifier = snapshot.char_classifier_at(position).for_completion(true);
        if trigger_in_words && classifier.is_word(char) {
            return true;
        }

        buffer.completion_triggers().contains(text)
    }
}

impl SemanticsProvider for Entity<Project> {
    fn hover(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        cx: &mut App,
    ) -> Option<Task<Vec<project::Hover>>> {
        Some(self.update(cx, |project, cx| project.hover(buffer, position, cx)))
    }

    fn document_highlights(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        cx: &mut App,
    ) -> Option<Task<Result<Vec<DocumentHighlight>>>> {
        Some(self.update(cx, |project, cx| {
            project.document_highlights(buffer, position, cx)
        }))
    }

    fn definitions(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        kind: GotoDefinitionKind,
        cx: &mut App,
    ) -> Option<Task<Result<Vec<LocationLink>>>> {
        Some(self.update(cx, |project, cx| match kind {
            GotoDefinitionKind::Symbol => project.definition(&buffer, position, cx),
            GotoDefinitionKind::Declaration => project.declaration(&buffer, position, cx),
            GotoDefinitionKind::Type => project.type_definition(&buffer, position, cx),
            GotoDefinitionKind::Implementation => project.implementation(&buffer, position, cx),
        }))
    }

    fn supports_inlay_hints(&self, buffer: &Entity<Buffer>, cx: &App) -> bool {
        // TODO: make this work for remote projects
        self.read(cx)
            .language_servers_for_local_buffer(buffer.read(cx), cx)
            .any(
                |(_, server)| match server.capabilities().inlay_hint_provider {
                    Some(lsp::OneOf::Left(enabled)) => enabled,
                    Some(lsp::OneOf::Right(_)) => true,
                    None => false,
                },
            )
    }

    fn inlay_hints(
        &self,
        buffer_handle: Entity<Buffer>,
        range: Range<text::Anchor>,
        cx: &mut App,
    ) -> Option<Task<anyhow::Result<Vec<InlayHint>>>> {
        Some(self.update(cx, |project, cx| {
            project.inlay_hints(buffer_handle, range, cx)
        }))
    }

    fn resolve_inlay_hint(
        &self,
        hint: InlayHint,
        buffer_handle: Entity<Buffer>,
        server_id: LanguageServerId,
        cx: &mut App,
    ) -> Option<Task<anyhow::Result<InlayHint>>> {
        Some(self.update(cx, |project, cx| {
            project.resolve_inlay_hint(hint, buffer_handle, server_id, cx)
        }))
    }

    fn range_for_rename(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        cx: &mut App,
    ) -> Option<Task<Result<Option<Range<text::Anchor>>>>> {
        Some(self.update(cx, |project, cx| {
            let buffer = buffer.clone();
            let task = project.prepare_rename(buffer.clone(), position, cx);
            cx.spawn(|_, mut cx| async move {
                Ok(match task.await? {
                    PrepareRenameResponse::Success(range) => Some(range),
                    PrepareRenameResponse::InvalidPosition => None,
                    PrepareRenameResponse::OnlyUnpreparedRenameSupported => {
                        // Fallback on using TreeSitter info to determine identifier range
                        buffer.update(&mut cx, |buffer, _| {
                            let snapshot = buffer.snapshot();
                            let (range, kind) = snapshot.surrounding_word(position);
                            if kind != Some(CharKind::Word) {
                                return None;
                            }
                            Some(
                                snapshot.anchor_before(range.start)
                                    ..snapshot.anchor_after(range.end),
                            )
                        })?
                    }
                })
            })
        }))
    }

    fn perform_rename(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        new_name: String,
        cx: &mut App,
    ) -> Option<Task<Result<ProjectTransaction>>> {
        Some(self.update(cx, |project, cx| {
            project.perform_rename(buffer.clone(), position, new_name, cx)
        }))
    }
}

fn inlay_hint_settings(
    location: Anchor,
    snapshot: &MultiBufferSnapshot,
    cx: &mut Context<Editor>,
) -> InlayHintSettings {
    let file = snapshot.file_at(location);
    let language = snapshot.language_at(location).map(|l| l.name());
    language_settings(language, file, cx).inlay_hints
}

fn consume_contiguous_rows(
    contiguous_row_selections: &mut Vec<Selection<Point>>,
    selection: &Selection<Point>,
    display_map: &DisplaySnapshot,
    selections: &mut Peekable<std::slice::Iter<Selection<Point>>>,
) -> (MultiBufferRow, MultiBufferRow) {
    contiguous_row_selections.push(selection.clone());
    let start_row = MultiBufferRow(selection.start.row);
    let mut end_row = ending_row(selection, display_map);

    while let Some(next_selection) = selections.peek() {
        if next_selection.start.row <= end_row.0 {
            end_row = ending_row(next_selection, display_map);
            contiguous_row_selections.push(selections.next().unwrap().clone());
        } else {
            break;
        }
    }
    (start_row, end_row)
}

fn ending_row(next_selection: &Selection<Point>, display_map: &DisplaySnapshot) -> MultiBufferRow {
    if next_selection.end.column > 0 || next_selection.is_empty() {
        MultiBufferRow(display_map.next_line_boundary(next_selection.end).0.row + 1)
    } else {
        MultiBufferRow(next_selection.end.row)
    }
}

impl EditorSnapshot {
    pub fn remote_selections_in_range<'a>(
        &'a self,
        range: &'a Range<Anchor>,
        collaboration_hub: &dyn CollaborationHub,
        cx: &'a App,
    ) -> impl 'a + Iterator<Item = RemoteSelection> {
        let participant_names = collaboration_hub.user_names(cx);
        let participant_indices = collaboration_hub.user_participant_indices(cx);
        let collaborators_by_peer_id = collaboration_hub.collaborators(cx);
        let collaborators_by_replica_id = collaborators_by_peer_id
            .iter()
            .map(|(_, collaborator)| (collaborator.replica_id, collaborator))
            .collect::<HashMap<_, _>>();
        self.buffer_snapshot
            .selections_in_range(range, false)
            .filter_map(move |(replica_id, line_mode, cursor_shape, selection)| {
                let collaborator = collaborators_by_replica_id.get(&replica_id)?;
                let participant_index = participant_indices.get(&collaborator.user_id).copied();
                let user_name = participant_names.get(&collaborator.user_id).cloned();
                Some(RemoteSelection {
                    replica_id,
                    selection,
                    cursor_shape,
                    line_mode,
                    participant_index,
                    peer_id: collaborator.peer_id,
                    user_name,
                })
            })
    }

    pub fn hunks_for_ranges(
        &self,
        ranges: impl Iterator<Item = Range<Point>>,
    ) -> Vec<MultiBufferDiffHunk> {
        let mut hunks = Vec::new();
        let mut processed_buffer_rows: HashMap<BufferId, HashSet<Range<text::Anchor>>> =
            HashMap::default();
        for query_range in ranges {
            let query_rows =
                MultiBufferRow(query_range.start.row)..MultiBufferRow(query_range.end.row + 1);
            for hunk in self.buffer_snapshot.diff_hunks_in_range(
                Point::new(query_rows.start.0, 0)..Point::new(query_rows.end.0, 0),
            ) {
                // Deleted hunk is an empty row range, no caret can be placed there and Zed allows to revert it
                // when the caret is just above or just below the deleted hunk.
                let allow_adjacent = hunk.status() == DiffHunkStatus::Removed;
                let related_to_selection = if allow_adjacent {
                    hunk.row_range.overlaps(&query_rows)
                        || hunk.row_range.start == query_rows.end
                        || hunk.row_range.end == query_rows.start
                } else {
                    hunk.row_range.overlaps(&query_rows)
                };
                if related_to_selection {
                    if !processed_buffer_rows
                        .entry(hunk.buffer_id)
                        .or_default()
                        .insert(hunk.buffer_range.start..hunk.buffer_range.end)
                    {
                        continue;
                    }
                    hunks.push(hunk);
                }
            }
        }

        hunks
    }

    pub fn language_at<T: ToOffset>(&self, position: T) -> Option<&Arc<Language>> {
        self.display_snapshot.buffer_snapshot.language_at(position)
    }

    pub fn is_focused(&self) -> bool {
        self.is_focused
    }

    pub fn placeholder_text(&self) -> Option<&Arc<str>> {
        self.placeholder_text.as_ref()
    }

    pub fn scroll_position(&self) -> gpui::Point<f32> {
        self.scroll_anchor.scroll_position(&self.display_snapshot)
    }

    fn gutter_dimensions(
        &self,
        font_id: FontId,
        font_size: Pixels,
        max_line_number_width: Pixels,
        cx: &App,
    ) -> Option<GutterDimensions> {
        if !self.show_gutter {
            return None;
        }

        let descent = cx.text_system().descent(font_id, font_size);
        let em_width = cx.text_system().em_width(font_id, font_size).log_err()?;
        let em_advance = cx.text_system().em_advance(font_id, font_size).log_err()?;

        let show_git_gutter = self.show_git_diff_gutter.unwrap_or_else(|| {
            matches!(
                ProjectSettings::get_global(cx).git.git_gutter,
                Some(GitGutterSetting::TrackedFiles)
            )
        });
        let gutter_settings = EditorSettings::get_global(cx).gutter;
        let show_line_numbers = self
            .show_line_numbers
            .unwrap_or(gutter_settings.line_numbers);
        let line_gutter_width = if show_line_numbers {
            // Avoid flicker-like gutter resizes when the line number gains another digit and only resize the gutter on files with N*10^5 lines.
            let min_width_for_number_on_gutter = em_advance * 4.0;
            max_line_number_width.max(min_width_for_number_on_gutter)
        } else {
            0.0.into()
        };

        let show_code_actions = self
            .show_code_actions
            .unwrap_or(gutter_settings.code_actions);

        let show_runnables = self.show_runnables.unwrap_or(gutter_settings.runnables);

        let git_blame_entries_width =
            self.git_blame_gutter_max_author_length
                .map(|max_author_length| {
                    const MAX_RELATIVE_TIMESTAMP: &str = "60 minutes ago";

                    /// The number of characters to dedicate to gaps and margins.
                    const SPACING_WIDTH: usize = 4;

                    let max_char_count = max_author_length
                        .min(GIT_BLAME_MAX_AUTHOR_CHARS_DISPLAYED)
                        + ::git::SHORT_SHA_LENGTH
                        + MAX_RELATIVE_TIMESTAMP.len()
                        + SPACING_WIDTH;

                    em_advance * max_char_count
                });

        let mut left_padding = git_blame_entries_width.unwrap_or(Pixels::ZERO);
        left_padding += if show_code_actions || show_runnables {
            em_width * 3.0
        } else if show_git_gutter && show_line_numbers {
            em_width * 2.0
        } else if show_git_gutter || show_line_numbers {
            em_width
        } else {
            px(0.)
        };

        let right_padding = if gutter_settings.folds && show_line_numbers {
            em_width * 4.0
        } else if gutter_settings.folds {
            em_width * 3.0
        } else if show_line_numbers {
            em_width
        } else {
            px(0.)
        };

        Some(GutterDimensions {
            left_padding,
            right_padding,
            width: line_gutter_width + left_padding + right_padding,
            margin: -descent,
            git_blame_entries_width,
        })
    }

    pub fn render_crease_toggle(
        &self,
        buffer_row: MultiBufferRow,
        row_contains_cursor: bool,
        editor: Entity<Editor>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let folded = self.is_line_folded(buffer_row);
        let mut is_foldable = false;

        if let Some(crease) = self
            .crease_snapshot
            .query_row(buffer_row, &self.buffer_snapshot)
        {
            is_foldable = true;
            match crease {
                Crease::Inline { render_toggle, .. } | Crease::Block { render_toggle, .. } => {
                    if let Some(render_toggle) = render_toggle {
                        let toggle_callback =
                            Arc::new(move |folded, window: &mut Window, cx: &mut App| {
                                if folded {
                                    editor.update(cx, |editor, cx| {
                                        editor.fold_at(&crate::FoldAt { buffer_row }, window, cx)
                                    });
                                } else {
                                    editor.update(cx, |editor, cx| {
                                        editor.unfold_at(
                                            &crate::UnfoldAt { buffer_row },
                                            window,
                                            cx,
                                        )
                                    });
                                }
                            });
                        return Some((render_toggle)(
                            buffer_row,
                            folded,
                            toggle_callback,
                            window,
                            cx,
                        ));
                    }
                }
            }
        }

        is_foldable |= self.starts_indent(buffer_row);

        if folded || (is_foldable && (row_contains_cursor || self.gutter_hovered)) {
            Some(
                Disclosure::new(("gutter_crease", buffer_row.0), !folded)
                    .toggle_state(folded)
                    .on_click(window.listener_for(&editor, move |this, _e, window, cx| {
                        if folded {
                            this.unfold_at(&UnfoldAt { buffer_row }, window, cx);
                        } else {
                            this.fold_at(&FoldAt { buffer_row }, window, cx);
                        }
                    }))
                    .into_any_element(),
            )
        } else {
            None
        }
    }

    pub fn render_crease_trailer(
        &self,
        buffer_row: MultiBufferRow,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let folded = self.is_line_folded(buffer_row);
        if let Crease::Inline { render_trailer, .. } = self
            .crease_snapshot
            .query_row(buffer_row, &self.buffer_snapshot)?
        {
            let render_trailer = render_trailer.as_ref()?;
            Some(render_trailer(buffer_row, folded, window, cx))
        } else {
            None
        }
    }
}

impl Deref for EditorSnapshot {
    type Target = DisplaySnapshot;

    fn deref(&self) -> &Self::Target {
        &self.display_snapshot
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorEvent {
    InputIgnored {
        text: Arc<str>,
    },
    InputHandled {
        utf16_range_to_replace: Option<Range<isize>>,
        text: Arc<str>,
    },
    ExcerptsAdded {
        buffer: Entity<Buffer>,
        predecessor: ExcerptId,
        excerpts: Vec<(ExcerptId, ExcerptRange<language::Anchor>)>,
    },
    ExcerptsRemoved {
        ids: Vec<ExcerptId>,
    },
    BufferFoldToggled {
        ids: Vec<ExcerptId>,
        folded: bool,
    },
    ExcerptsEdited {
        ids: Vec<ExcerptId>,
    },
    ExcerptsExpanded {
        ids: Vec<ExcerptId>,
    },
    BufferEdited,
    Edited {
        transaction_id: clock::Lamport,
    },
    Reparsed(BufferId),
    Focused,
    FocusedIn,
    Blurred,
    DirtyChanged,
    Saved,
    TitleChanged,
    DiffBaseChanged,
    SelectionsChanged {
        local: bool,
    },
    ScrollPositionChanged {
        local: bool,
        autoscroll: bool,
    },
    Closed,
    TransactionUndone {
        transaction_id: clock::Lamport,
    },
    TransactionBegun {
        transaction_id: clock::Lamport,
    },
    Reloaded,
    CursorShapeChanged,
}

impl EventEmitter<EditorEvent> for Editor {}

impl Focusable for Editor {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Editor {
    fn render<'a>(&mut self, _: &mut Window, cx: &mut Context<'a, Self>) -> impl IntoElement {
        let settings = ThemeSettings::get_global(cx);

        let mut text_style = match self.mode {
            EditorMode::SingleLine { .. } | EditorMode::AutoHeight { .. } => TextStyle {
                color: cx.theme().colors().editor_foreground,
                font_family: settings.ui_font.family.clone(),
                font_features: settings.ui_font.features.clone(),
                font_fallbacks: settings.ui_font.fallbacks.clone(),
                font_size: rems(0.875).into(),
                font_weight: settings.ui_font.weight,
                line_height: relative(settings.buffer_line_height.value()),
                ..Default::default()
            },
            EditorMode::Full => TextStyle {
                color: cx.theme().colors().editor_foreground,
                font_family: settings.buffer_font.family.clone(),
                font_features: settings.buffer_font.features.clone(),
                font_fallbacks: settings.buffer_font.fallbacks.clone(),
                font_size: settings.buffer_font_size().into(),
                font_weight: settings.buffer_font.weight,
                line_height: relative(settings.buffer_line_height.value()),
                ..Default::default()
            },
        };
        if let Some(text_style_refinement) = &self.text_style_refinement {
            text_style.refine(text_style_refinement)
        }

        let background = match self.mode {
            EditorMode::SingleLine { .. } => cx.theme().system().transparent,
            EditorMode::AutoHeight { max_lines: _ } => cx.theme().system().transparent,
            EditorMode::Full => cx.theme().colors().editor_background,
        };

        EditorElement::new(
            &cx.entity(),
            EditorStyle {
                background,
                local_player: cx.theme().players().local(),
                text: text_style,
                scrollbar_width: EditorElement::SCROLLBAR_WIDTH,
                syntax: cx.theme().syntax().clone(),
                status: cx.theme().status().clone(),
                inlay_hints_style: make_inlay_hints_style(cx),
                inline_completion_styles: make_suggestion_styles(cx),
                unnecessary_code_fade: ThemeSettings::get_global(cx).unnecessary_code_fade,
            },
        )
    }
}

impl EntityInputHandler for Editor {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let snapshot = self.buffer.read(cx).read(cx);
        let start = snapshot.clip_offset_utf16(OffsetUtf16(range_utf16.start), Bias::Left);
        let end = snapshot.clip_offset_utf16(OffsetUtf16(range_utf16.end), Bias::Right);
        if (start.0..end.0) != range_utf16 {
            adjusted_range.replace(start.0..end.0);
        }
        Some(snapshot.text_for_range(start..end).collect())
    }

    fn selected_text_range(
        &mut self,
        ignore_disabled_input: bool,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        // Prevent the IME menu from appearing when holding down an alphabetic key
        // while input is disabled.
        if !ignore_disabled_input && !self.input_enabled {
            return None;
        }

        let selection = self.selections.newest::<OffsetUtf16>(cx);
        let range = selection.range();

        Some(UTF16Selection {
            range: range.start.0..range.end.0,
            reversed: selection.reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, cx: &mut Context<Self>) -> Option<Range<usize>> {
        let snapshot = self.buffer.read(cx).read(cx);
        let range = self.text_highlights::<InputComposition>(cx)?.1.first()?;
        Some(range.start.to_offset_utf16(&snapshot).0..range.end.to_offset_utf16(&snapshot).0)
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.clear_highlights::<InputComposition>(cx);
        self.ime_transaction.take();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_enabled {
            cx.emit(EditorEvent::InputIgnored { text: text.into() });
            return;
        }

        self.transact(window, cx, |this, window, cx| {
            let new_selected_ranges = if let Some(range_utf16) = range_utf16 {
                let range_utf16 = OffsetUtf16(range_utf16.start)..OffsetUtf16(range_utf16.end);
                Some(this.selection_replacement_ranges(range_utf16, cx))
            } else {
                this.marked_text_ranges(cx)
            };

            let range_to_replace = new_selected_ranges.as_ref().and_then(|ranges_to_replace| {
                let newest_selection_id = this.selections.newest_anchor().id;
                this.selections
                    .all::<OffsetUtf16>(cx)
                    .iter()
                    .zip(ranges_to_replace.iter())
                    .find_map(|(selection, range)| {
                        if selection.id == newest_selection_id {
                            Some(
                                (range.start.0 as isize - selection.head().0 as isize)
                                    ..(range.end.0 as isize - selection.head().0 as isize),
                            )
                        } else {
                            None
                        }
                    })
            });

            cx.emit(EditorEvent::InputHandled {
                utf16_range_to_replace: range_to_replace,
                text: text.into(),
            });

            if let Some(new_selected_ranges) = new_selected_ranges {
                this.change_selections(None, window, cx, |selections| {
                    selections.select_ranges(new_selected_ranges)
                });
                this.backspace(&Default::default(), window, cx);
            }

            this.handle_input(text, window, cx);
        });

        if let Some(transaction) = self.ime_transaction {
            self.buffer.update(cx, |buffer, cx| {
                buffer.group_until_transaction(transaction, cx);
            });
        }

        self.unmark_text(window, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_enabled {
            return;
        }

        let transaction = self.transact(window, cx, |this, window, cx| {
            let ranges_to_replace = if let Some(mut marked_ranges) = this.marked_text_ranges(cx) {
                let snapshot = this.buffer.read(cx).read(cx);
                if let Some(relative_range_utf16) = range_utf16.as_ref() {
                    for marked_range in &mut marked_ranges {
                        marked_range.end.0 = marked_range.start.0 + relative_range_utf16.end;
                        marked_range.start.0 += relative_range_utf16.start;
                        marked_range.start =
                            snapshot.clip_offset_utf16(marked_range.start, Bias::Left);
                        marked_range.end =
                            snapshot.clip_offset_utf16(marked_range.end, Bias::Right);
                    }
                }
                Some(marked_ranges)
            } else if let Some(range_utf16) = range_utf16 {
                let range_utf16 = OffsetUtf16(range_utf16.start)..OffsetUtf16(range_utf16.end);
                Some(this.selection_replacement_ranges(range_utf16, cx))
            } else {
                None
            };

            let range_to_replace = ranges_to_replace.as_ref().and_then(|ranges_to_replace| {
                let newest_selection_id = this.selections.newest_anchor().id;
                this.selections
                    .all::<OffsetUtf16>(cx)
                    .iter()
                    .zip(ranges_to_replace.iter())
                    .find_map(|(selection, range)| {
                        if selection.id == newest_selection_id {
                            Some(
                                (range.start.0 as isize - selection.head().0 as isize)
                                    ..(range.end.0 as isize - selection.head().0 as isize),
                            )
                        } else {
                            None
                        }
                    })
            });

            cx.emit(EditorEvent::InputHandled {
                utf16_range_to_replace: range_to_replace,
                text: text.into(),
            });

            if let Some(ranges) = ranges_to_replace {
                this.change_selections(None, window, cx, |s| s.select_ranges(ranges));
            }

            let marked_ranges = {
                let snapshot = this.buffer.read(cx).read(cx);
                this.selections
                    .disjoint_anchors()
                    .iter()
                    .map(|selection| {
                        selection.start.bias_left(&snapshot)..selection.end.bias_right(&snapshot)
                    })
                    .collect::<Vec<_>>()
            };

            if text.is_empty() {
                this.unmark_text(window, cx);
            } else {
                this.highlight_text::<InputComposition>(
                    marked_ranges.clone(),
                    HighlightStyle {
                        underline: Some(UnderlineStyle {
                            thickness: px(1.),
                            color: None,
                            wavy: false,
                        }),
                        ..Default::default()
                    },
                    cx,
                );
            }

            // Disable auto-closing when composing text (i.e. typing a `"` on a Brazilian keyboard)
            let use_autoclose = this.use_autoclose;
            let use_auto_surround = this.use_auto_surround;
            this.set_use_autoclose(false);
            this.set_use_auto_surround(false);
            this.handle_input(text, window, cx);
            this.set_use_autoclose(use_autoclose);
            this.set_use_auto_surround(use_auto_surround);

            if let Some(new_selected_range) = new_selected_range_utf16 {
                let snapshot = this.buffer.read(cx).read(cx);
                let new_selected_ranges = marked_ranges
                    .into_iter()
                    .map(|marked_range| {
                        let insertion_start = marked_range.start.to_offset_utf16(&snapshot).0;
                        let new_start = OffsetUtf16(new_selected_range.start + insertion_start);
                        let new_end = OffsetUtf16(new_selected_range.end + insertion_start);
                        snapshot.clip_offset_utf16(new_start, Bias::Left)
                            ..snapshot.clip_offset_utf16(new_end, Bias::Right)
                    })
                    .collect::<Vec<_>>();

                drop(snapshot);
                this.change_selections(None, window, cx, |selections| {
                    selections.select_ranges(new_selected_ranges)
                });
            }
        });

        self.ime_transaction = self.ime_transaction.or(transaction);
        if let Some(transaction) = self.ime_transaction {
            self.buffer.update(cx, |buffer, cx| {
                buffer.group_until_transaction(transaction, cx);
            });
        }

        if self.text_highlights::<InputComposition>(cx).is_none() {
            self.ime_transaction.take();
        }
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: gpui::Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Bounds<Pixels>> {
        let text_layout_details = self.text_layout_details(window);
        let gpui::Size {
            width: em_width,
            height: line_height,
        } = self.character_size(window);

        let snapshot = self.snapshot(window, cx);
        let scroll_position = snapshot.scroll_position();
        let scroll_left = scroll_position.x * em_width;

        let start = OffsetUtf16(range_utf16.start).to_display_point(&snapshot);
        let x = snapshot.x_for_display_point(start, &text_layout_details) - scroll_left
            + self.gutter_dimensions.width
            + self.gutter_dimensions.margin;
        let y = line_height * (start.row().as_f32() - scroll_position.y);

        Some(Bounds {
            origin: element_bounds.origin + point(x, y),
            size: size(em_width, line_height),
        })
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let position_map = self.last_position_map.as_ref()?;
        if !position_map.text_hitbox.contains(&point) {
            return None;
        }
        let display_point = position_map.point_for_position(point).previous_valid;
        let anchor = position_map
            .snapshot
            .display_point_to_anchor(display_point, Bias::Left);
        let utf16_offset = anchor.to_offset_utf16(&position_map.snapshot.buffer_snapshot);
        Some(utf16_offset.0)
    }
}

trait SelectionExt {
    fn display_range(&self, map: &DisplaySnapshot) -> Range<DisplayPoint>;
    fn spanned_rows(
        &self,
        include_end_if_at_line_start: bool,
        map: &DisplaySnapshot,
    ) -> Range<MultiBufferRow>;
}

impl<T: ToPoint + ToOffset> SelectionExt for Selection<T> {
    fn display_range(&self, map: &DisplaySnapshot) -> Range<DisplayPoint> {
        let start = self
            .start
            .to_point(&map.buffer_snapshot)
            .to_display_point(map);
        let end = self
            .end
            .to_point(&map.buffer_snapshot)
            .to_display_point(map);
        if self.reversed {
            end..start
        } else {
            start..end
        }
    }

    fn spanned_rows(
        &self,
        include_end_if_at_line_start: bool,
        map: &DisplaySnapshot,
    ) -> Range<MultiBufferRow> {
        let start = self.start.to_point(&map.buffer_snapshot);
        let mut end = self.end.to_point(&map.buffer_snapshot);
        if !include_end_if_at_line_start && start.row != end.row && end.column == 0 {
            end.row -= 1;
        }

        let buffer_start = map.prev_line_boundary(start).0;
        let buffer_end = map.next_line_boundary(end).0;
        MultiBufferRow(buffer_start.row)..MultiBufferRow(buffer_end.row + 1)
    }
}

impl<T: InvalidationRegion> InvalidationStack<T> {
    fn invalidate<S>(&mut self, selections: &[Selection<S>], buffer: &MultiBufferSnapshot)
    where
        S: Clone + ToOffset,
    {
        while let Some(region) = self.last() {
            let all_selections_inside_invalidation_ranges =
                if selections.len() == region.ranges().len() {
                    selections
                        .iter()
                        .zip(region.ranges().iter().map(|r| r.to_offset(buffer)))
                        .all(|(selection, invalidation_range)| {
                            let head = selection.head().to_offset(buffer);
                            invalidation_range.start <= head && invalidation_range.end >= head
                        })
                } else {
                    false
                };

            if all_selections_inside_invalidation_ranges {
                break;
            } else {
                self.pop();
            }
        }
    }
}

impl<T> Default for InvalidationStack<T> {
    fn default() -> Self {
        Self(Default::default())
    }
}

impl<T> Deref for InvalidationStack<T> {
    type Target = Vec<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for InvalidationStack<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl InvalidationRegion for SnippetState {
    fn ranges(&self) -> &[Range<Anchor>] {
        &self.ranges[self.active_index]
    }
}

pub fn diagnostic_block_renderer(
    diagnostic: Diagnostic,
    max_message_rows: Option<u8>,
    allow_closing: bool,
    _is_valid: bool,
) -> RenderBlock {
    let (text_without_backticks, code_ranges) =
        highlight_diagnostic_message(&diagnostic, max_message_rows);

    Arc::new(move |cx: &mut BlockContext| {
        let group_id: SharedString = cx.block_id.to_string().into();

        let mut text_style = cx.window.text_style().clone();
        text_style.color = diagnostic_style(diagnostic.severity, cx.theme().status());
        let theme_settings = ThemeSettings::get_global(cx);
        text_style.font_family = theme_settings.buffer_font.family.clone();
        text_style.font_style = theme_settings.buffer_font.style;
        text_style.font_features = theme_settings.buffer_font.features.clone();
        text_style.font_weight = theme_settings.buffer_font.weight;

        let multi_line_diagnostic = diagnostic.message.contains('\n');

        let buttons = |diagnostic: &Diagnostic| {
            if multi_line_diagnostic {
                v_flex()
            } else {
                h_flex()
            }
            .when(allow_closing, |div| {
                div.children(diagnostic.is_primary.then(|| {
                    IconButton::new("close-block", IconName::XCircle)
                        .icon_color(Color::Muted)
                        .size(ButtonSize::Compact)
                        .style(ButtonStyle::Transparent)
                        .visible_on_hover(group_id.clone())
                        .on_click(move |_click, window, cx| {
                            window.dispatch_action(Box::new(Cancel), cx)
                        })
                        .tooltip(|window, cx| {
                            Tooltip::for_action("Close Diagnostics", &Cancel, window, cx)
                        })
                }))
            })
            .child(
                IconButton::new("copy-block", IconName::Copy)
                    .icon_color(Color::Muted)
                    .size(ButtonSize::Compact)
                    .style(ButtonStyle::Transparent)
                    .visible_on_hover(group_id.clone())
                    .on_click({
                        let message = diagnostic.message.clone();
                        move |_click, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(message.clone()))
                        }
                    })
                    .tooltip(Tooltip::text("Copy diagnostic message")),
            )
        };

        let icon_size = buttons(&diagnostic).into_any_element().layout_as_root(
            AvailableSpace::min_size(),
            cx.window,
            cx.app,
        );

        h_flex()
            .id(cx.block_id)
            .group(group_id.clone())
            .relative()
            .size_full()
            .block_mouse_down()
            .pl(cx.gutter_dimensions.width)
            .w(cx.max_width - cx.gutter_dimensions.full_width())
            .child(
                div()
                    .flex()
                    .w(cx.anchor_x - cx.gutter_dimensions.width - icon_size.width)
                    .flex_shrink(),
            )
            .child(buttons(&diagnostic))
            .child(div().flex().flex_shrink_0().child(
                StyledText::new(text_without_backticks.clone()).with_highlights(
                    &text_style,
                    code_ranges.iter().map(|range| {
                        (
                            range.clone(),
                            HighlightStyle {
                                font_weight: Some(FontWeight::BOLD),
                                ..Default::default()
                            },
                        )
                    }),
                ),
            ))
            .into_any_element()
    })
}

fn inline_completion_edit_text(
    current_snapshot: &BufferSnapshot,
    edits: &[(Range<Anchor>, String)],
    edit_preview: &EditPreview,
    include_deletions: bool,
    cx: &App,
) -> HighlightedText {
    let edits = edits
        .iter()
        .map(|(anchor, text)| {
            (
                anchor.start.text_anchor..anchor.end.text_anchor,
                text.clone(),
            )
        })
        .collect::<Vec<_>>();

    edit_preview.highlight_edits(current_snapshot, &edits, include_deletions, cx)
}

pub fn highlight_diagnostic_message(
    diagnostic: &Diagnostic,
    mut max_message_rows: Option<u8>,
) -> (SharedString, Vec<Range<usize>>) {
    let mut text_without_backticks = String::new();
    let mut code_ranges = Vec::new();

    if let Some(source) = &diagnostic.source {
        text_without_backticks.push_str(source);
        code_ranges.push(0..source.len());
        text_without_backticks.push_str(": ");
    }

    let mut prev_offset = 0;
    let mut in_code_block = false;
    let has_row_limit = max_message_rows.is_some();
    let mut newline_indices = diagnostic
        .message
        .match_indices('\n')
        .filter(|_| has_row_limit)
        .map(|(ix, _)| ix)
        .fuse()
        .peekable();

    for (quote_ix, _) in diagnostic
        .message
        .match_indices('`')
        .chain([(diagnostic.message.len(), "")])
    {
        let mut first_newline_ix = None;
        let mut last_newline_ix = None;
        while let Some(newline_ix) = newline_indices.peek() {
            if *newline_ix < quote_ix {
                if first_newline_ix.is_none() {
                    first_newline_ix = Some(*newline_ix);
                }
                last_newline_ix = Some(*newline_ix);

                if let Some(rows_left) = &mut max_message_rows {
                    if *rows_left == 0 {
                        break;
                    } else {
                        *rows_left -= 1;
                    }
                }
                let _ = newline_indices.next();
            } else {
                break;
            }
        }
        let prev_len = text_without_backticks.len();
        let new_text = &diagnostic.message[prev_offset..first_newline_ix.unwrap_or(quote_ix)];
        text_without_backticks.push_str(new_text);
        if in_code_block {
            code_ranges.push(prev_len..text_without_backticks.len());
        }
        prev_offset = last_newline_ix.unwrap_or(quote_ix) + 1;
        in_code_block = !in_code_block;
        if first_newline_ix.map_or(false, |newline_ix| newline_ix < quote_ix) {
            text_without_backticks.push_str("...");
            break;
        }
    }

    (text_without_backticks.into(), code_ranges)
}

fn diagnostic_style(severity: DiagnosticSeverity, colors: &StatusColors) -> Hsla {
    match severity {
        DiagnosticSeverity::ERROR => colors.error,
        DiagnosticSeverity::WARNING => colors.warning,
        DiagnosticSeverity::INFORMATION => colors.info,
        DiagnosticSeverity::HINT => colors.info,
        _ => colors.ignored,
    }
}

pub fn styled_runs_for_code_label<'a>(
    label: &'a CodeLabel,
    syntax_theme: &'a theme::SyntaxTheme,
) -> impl 'a + Iterator<Item = (Range<usize>, HighlightStyle)> {
    let fade_out = HighlightStyle {
        fade_out: Some(0.35),
        ..Default::default()
    };

    let mut prev_end = label.filter_range.end;
    label
        .runs
        .iter()
        .enumerate()
        .flat_map(move |(ix, (range, highlight_id))| {
            let style = if let Some(style) = highlight_id.style(syntax_theme) {
                style
            } else {
                return Default::default();
            };
            let mut muted_style = style;
            muted_style.highlight(fade_out);

            let mut runs = SmallVec::<[(Range<usize>, HighlightStyle); 3]>::new();
            if range.start >= label.filter_range.end {
                if range.start > prev_end {
                    runs.push((prev_end..range.start, fade_out));
                }
                runs.push((range.clone(), muted_style));
            } else if range.end <= label.filter_range.end {
                runs.push((range.clone(), style));
            } else {
                runs.push((range.start..label.filter_range.end, style));
                runs.push((label.filter_range.end..range.end, muted_style));
            }
            prev_end = cmp::max(prev_end, range.end);

            if ix + 1 == label.runs.len() && label.text.len() > prev_end {
                runs.push((prev_end..label.text.len(), fade_out));
            }

            runs
        })
}

pub(crate) fn split_words(text: &str) -> impl std::iter::Iterator<Item = &str> + '_ {
    let mut prev_index = 0;
    let mut prev_codepoint: Option<char> = None;
    text.char_indices()
        .chain([(text.len(), '\0')])
        .filter_map(move |(index, codepoint)| {
            let prev_codepoint = prev_codepoint.replace(codepoint)?;
            let is_boundary = index == text.len()
                || !prev_codepoint.is_uppercase() && codepoint.is_uppercase()
                || !prev_codepoint.is_alphanumeric() && codepoint.is_alphanumeric();
            if is_boundary {
                let chunk = &text[prev_index..index];
                prev_index = index;
                Some(chunk)
            } else {
                None
            }
        })
}

pub trait RangeToAnchorExt: Sized {
    fn to_anchors(self, snapshot: &MultiBufferSnapshot) -> Range<Anchor>;

    fn to_display_points(self, snapshot: &EditorSnapshot) -> Range<DisplayPoint> {
        let anchor_range = self.to_anchors(&snapshot.buffer_snapshot);
        anchor_range.start.to_display_point(snapshot)..anchor_range.end.to_display_point(snapshot)
    }
}

impl<T: ToOffset> RangeToAnchorExt for Range<T> {
    fn to_anchors(self, snapshot: &MultiBufferSnapshot) -> Range<Anchor> {
        let start_offset = self.start.to_offset(snapshot);
        let end_offset = self.end.to_offset(snapshot);
        if start_offset == end_offset {
            snapshot.anchor_before(start_offset)..snapshot.anchor_before(end_offset)
        } else {
            snapshot.anchor_after(self.start)..snapshot.anchor_before(self.end)
        }
    }
}

pub trait RowExt {
    fn as_f32(&self) -> f32;

    fn next_row(&self) -> Self;

    fn previous_row(&self) -> Self;

    fn minus(&self, other: Self) -> u32;
}

impl RowExt for DisplayRow {
    fn as_f32(&self) -> f32 {
        self.0 as f32
    }

    fn next_row(&self) -> Self {
        Self(self.0 + 1)
    }

    fn previous_row(&self) -> Self {
        Self(self.0.saturating_sub(1))
    }

    fn minus(&self, other: Self) -> u32 {
        self.0 - other.0
    }
}

impl RowExt for MultiBufferRow {
    fn as_f32(&self) -> f32 {
        self.0 as f32
    }

    fn next_row(&self) -> Self {
        Self(self.0 + 1)
    }

    fn previous_row(&self) -> Self {
        Self(self.0.saturating_sub(1))
    }

    fn minus(&self, other: Self) -> u32 {
        self.0 - other.0
    }
}

trait RowRangeExt {
    type Row;

    fn len(&self) -> usize;

    fn iter_rows(&self) -> impl DoubleEndedIterator<Item = Self::Row>;
}

impl RowRangeExt for Range<MultiBufferRow> {
    type Row = MultiBufferRow;

    fn len(&self) -> usize {
        (self.end.0 - self.start.0) as usize
    }

    fn iter_rows(&self) -> impl DoubleEndedIterator<Item = MultiBufferRow> {
        (self.start.0..self.end.0).map(MultiBufferRow)
    }
}

impl RowRangeExt for Range<DisplayRow> {
    type Row = DisplayRow;

    fn len(&self) -> usize {
        (self.end.0 - self.start.0) as usize
    }

    fn iter_rows(&self) -> impl DoubleEndedIterator<Item = DisplayRow> {
        (self.start.0..self.end.0).map(DisplayRow)
    }
}

/// If select range has more than one line, we
/// just point the cursor to range.start.
fn collapse_multiline_range(range: Range<Point>) -> Range<Point> {
    if range.start.row == range.end.row {
        range
    } else {
        range.start..range.start
    }
}
pub struct KillRing(ClipboardItem);
impl Global for KillRing {}

const UPDATE_DEBOUNCE: Duration = Duration::from_millis(50);

fn all_edits_insertions_or_deletions(
    edits: &Vec<(Range<Anchor>, String)>,
    snapshot: &MultiBufferSnapshot,
) -> bool {
    let mut all_insertions = true;
    let mut all_deletions = true;

    for (range, new_text) in edits.iter() {
        let range_is_empty = range.to_offset(&snapshot).is_empty();
        let text_is_empty = new_text.is_empty();

        if range_is_empty != text_is_empty {
            if range_is_empty {
                all_deletions = false;
            } else {
                all_insertions = false;
            }
        } else {
            return false;
        }

        if !all_insertions && !all_deletions {
            return false;
        }
    }
    all_insertions || all_deletions
}
