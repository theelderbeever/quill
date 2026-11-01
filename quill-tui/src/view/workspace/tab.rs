//! One tab per saved query, each with its own editor, results and run.
use std::ops::Range;

use ratatui::text::{Line, Span};

use super::{editor::Editor, results::Results, stats::Stats};
use crate::{
    action::Action,
    component::{Context, KeyOutcome},
    connection::{ConnectionId, ConnectionState},
    format,
    store::queries::{QueryName, QueryStore},
    task::{RunId, RunMsg, run::Run, stats as stats_task},
    widget::{palette::Flash, theme},
};

pub struct QueryTab {
    query: QueryName,
    pub editor: Editor,
    pub results: Results,
    pub stats: Stats,
    run: Option<Run>,
}

impl QueryTab {
    /// Open `query`, resuming its saved text.
    pub fn open(query: QueryName, store: &QueryStore) -> anyhow::Result<Self> {
        let editor = Editor::new(store.read(&query)?)?;
        Ok(Self::new(query, editor))
    }

    pub fn new(query: QueryName, editor: Editor) -> Self {
        Self {
            query,
            editor,
            results: Results::new(),
            stats: Stats::new(),
            run: None,
        }
    }

    pub const fn query(&self) -> &QueryName {
        &self.query
    }

    pub const fn is_running(&self) -> bool {
        self.run.is_some()
    }

    /// Whether updates for `run` belong to this tab.
    pub fn owns(&self, run: RunId) -> bool {
        self.run.as_ref().is_some_and(|r| r.id() == run)
            || self.results.owns(run)
            || self.stats.run() == Some(run)
    }

    pub fn cancel(&self) -> bool {
        self.run.as_ref().inspect(|r| r.cancel()).is_some()
    }

    pub fn save_if(
        &mut self,
        store: &QueryStore,
        text: impl FnOnce(&mut Editor) -> Option<String>,
    ) -> anyhow::Result<()> {
        if let Some(text) = text(&mut self.editor) {
            store.write(&self.query, &text)?;
            self.editor.mark_saved(text);
        }
        Ok(())
    }

    pub fn start_run(&mut self, connection: &ConnectionId, ctx: &Context) -> KeyOutcome {
        let report = |message: String| KeyOutcome::Action(Action::Report(message));
        if self.run.is_some() {
            return report("a query is already running; esc cancels it".into());
        }
        let connections = ctx.connections();
        let Some(pool) = connections.pool(connection) else {
            return report(match connections.state(connection) {
                Some(ConnectionState::Failed(error)) => format!("{connection}: {error}"),
                _ => format!("{connection} is still connecting"),
            });
        };
        let sql = self.editor.sql_to_run();
        if sql.trim().is_empty() {
            return report("nothing to run".into());
        }
        let run = Run::start(
            pool,
            sql,
            &ctx.shared.results_dir,
            ctx.options().byte_limit,
            ctx.shared.messages.clone(),
        );
        self.stats.started(run.id());
        self.run = Some(run);
        self.results.running();
        KeyOutcome::Consumed
    }

    /// Apply an update for one of this tab's runs. Returns a message to show.
    pub fn handle_msg(
        &mut self,
        msg: RunMsg,
        connection: &ConnectionId,
        ctx: &Context,
    ) -> Option<Flash> {
        match msg {
            RunMsg::Started {
                run,
                query,
                control,
            } => {
                if let Some(current) = self.run.as_mut().filter(|r| r.id() == run) {
                    current.attach(query, control);
                }
            }
            RunMsg::Finished { run, result } => {
                if self.run.as_ref().is_some_and(|r| r.id() == run)
                    && let Some(finished) = self.run.take()
                {
                    let (client, sink) = finished.metrics();
                    let pool = ctx.connections().pool(connection);
                    let lookup = finished.query().zip(pool);
                    self.stats.finished(run, client, sink, lookup.is_some());
                    if let Some((query, pool)) = lookup {
                        stats_task::spawn_server_metrics(
                            pool,
                            query,
                            run,
                            ctx.shared.messages.clone(),
                        );
                    }
                    self.results.finish(run, finished.dir(), result, ctx);
                }
            }
            RunMsg::Page { run, index, result } => return self.results.page(run, index, result),
            RunMsg::Profile { run, result } => self.stats.profile(run, result),
            RunMsg::ServerMetrics { run, result } => self.stats.server(run, result),
            RunMsg::Derived { base, view, result } => {
                return self.results.derived(base, view, result, ctx);
            }
            RunMsg::Saved { path, result } => {
                return Some(match result {
                    Ok(()) => Flash::Info(format!("saved {}", path.display())),
                    Err(e) => Flash::Error(format!("save failed: {e:#}")),
                });
            }
        }
        None
    }

    /// Run progress, shown on the palette line.
    pub fn status(&self) -> Option<Line<'_>> {
        let (sink, elapsed) = self.run.as_ref()?.progress();
        Some(Line::from(vec![
            Span::styled("running ", theme::PENDING),
            Span::raw(format!(
                "{} rows · {} · {}",
                format::count(sink.rows_written),
                format::bytes(sink.bytes_written),
                format::duration(elapsed),
            )),
            Span::styled("  esc cancel", theme::DIM),
        ]))
    }

    /// The tab's title: ` name ` or ` name● ` while running.
    fn title(&self, active: bool) -> Vec<Span<'static>> {
        let style = if active {
            theme::ACTIVE_TAB
        } else {
            theme::DIM
        };
        let mut spans = vec![Span::styled(format!(" {}", self.query), style)];
        if self.is_running() {
            spans.push(Span::styled(theme::RUNNING_MARKER, theme::PENDING));
        }
        spans.push(Span::styled(" ", style));
        spans
    }
}

/// Tabs sorted by query name. Never empty, and `active` is always in range.
pub struct Tabs {
    tabs: Vec<QueryTab>,
    active: usize,
}

impl Tabs {
    pub fn new(first: QueryTab) -> Self {
        Self {
            tabs: vec![first],
            active: 0,
        }
    }

    pub fn active(&self) -> &QueryTab {
        &self.tabs[self.active]
    }

    pub fn active_mut(&mut self) -> &mut QueryTab {
        &mut self.tabs[self.active]
    }

    pub const fn next(&mut self) {
        self.active = (self.active + 1) % self.tabs.len();
    }

    pub const fn prev(&mut self) {
        self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
    }

    /// Select the tab at `index`. Returns false if it is out of range.
    pub const fn select_index(&mut self, index: usize) -> bool {
        let found = index < self.tabs.len();
        if found {
            self.active = index;
        }
        found
    }

    pub const fn len(&self) -> usize {
        self.tabs.len()
    }

    /// Add a tab for `query` and select it.
    pub fn add(&mut self, query: QueryName, store: &QueryStore) -> anyhow::Result<()> {
        let at = self.tabs.partition_point(|t| t.query() < &query);
        if self.tabs.get(at).is_none_or(|t| t.query() != &query) {
            self.tabs.insert(at, QueryTab::open(query, store)?);
        }
        self.active = at;
        Ok(())
    }

    /// Drop the tab for `query`, stopping its run. The only tab stays, so
    /// this returns false for it.
    pub fn remove(&mut self, query: &QueryName) -> bool {
        let found = self.tabs.iter().position(|t| t.query() == query);
        let Some(index) = found.filter(|_| self.tabs.len() > 1) else {
            return false;
        };
        self.tabs.remove(index);
        if self.active > index {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        true
    }

    pub fn iter(&self) -> impl Iterator<Item = &QueryTab> {
        self.tabs.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut QueryTab> {
        self.tabs.iter_mut()
    }

    pub fn find_run_mut(&mut self, run: RunId) -> Option<&mut QueryTab> {
        self.tabs.iter_mut().find(|t| t.owns(run))
    }

    /// Match the saved queries in `names` and select `active`, which gets a
    /// tab even when it is not among them. Tabs whose file is gone are
    /// dropped unless their query is still running.
    pub fn sync(
        &mut self,
        names: &[QueryName],
        active: &QueryName,
        store: &QueryStore,
    ) -> anyhow::Result<()> {
        let keep = |q: &QueryName| q == active || names.contains(q);
        self.tabs.retain(|t| t.is_running() || keep(t.query()));
        for name in names {
            self.add(name.clone(), store)?;
        }
        self.add(active.clone(), store)
    }

    /// The tab line for a border `width` cells wide, starting at the first
    /// tab that keeps the active one in view. Also returns each drawn tab's
    /// span of columns from the line's start.
    pub fn title(&self, width: u16) -> (Line<'static>, Vec<(Range<u16>, usize)>) {
        let separator = Span::width(&Span::raw(theme::TAB_SEPARATOR));
        let titles: Vec<Vec<Span<'static>>> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| t.title(i == self.active))
            .collect();
        let widths: Vec<usize> = titles
            .iter()
            .map(|spans| spans.iter().map(Span::width).sum())
            .collect();
        let width = usize::from(width);
        let fits = |range: Range<usize>| {
            let gaps = range.len().saturating_sub(1) * separator;
            widths[range].iter().sum::<usize>() + gaps <= width
        };
        let start = (0..self.active)
            .find(|&s| fits(s..self.active + 1))
            .unwrap_or(self.active);

        let mut line = Line::default();
        let mut hits = Vec::new();
        let mut x = 0;
        for (index, spans) in titles.into_iter().enumerate().skip(start) {
            if index > self.active && !fits(start..index + 1) {
                break;
            }
            if index > start {
                line.push_span(Span::styled(theme::TAB_SEPARATOR, theme::BORDER));
                x += separator;
            }
            let end = x + widths[index];
            hits.push((to_u16(x)..to_u16(end), index));
            line.extend(spans);
            x = end;
        }
        (line, hits)
    }
}

fn to_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(n: &str) -> QueryName {
        QueryName::parse(n).unwrap()
    }

    fn tab(n: &str) -> QueryTab {
        QueryTab::new(name(n), Editor::new(String::new()).unwrap())
    }

    fn names(tabs: &Tabs) -> Vec<&str> {
        tabs.iter().map(|t| t.query().as_str()).collect()
    }

    fn store() -> QueryStore {
        QueryStore::new(std::env::temp_dir().join("quill-test-none"))
    }

    fn three() -> Tabs {
        let mut tabs = Tabs::new(tab("b"));
        tabs.sync(&[name("a"), name("b"), name("c")], &name("b"), &store())
            .unwrap();
        tabs
    }

    #[test]
    fn sync_adds_tabs_in_name_order_and_selects() {
        let tabs = three();
        assert_eq!(names(&tabs), ["a", "b", "c"]);
        assert_eq!(tabs.active().query().as_str(), "b");
    }

    #[test]
    fn next_and_prev_wrap() {
        let mut tabs = three();
        tabs.next();
        tabs.next();
        assert_eq!(tabs.active().query().as_str(), "a");
        tabs.prev();
        assert_eq!(tabs.active().query().as_str(), "c");
        assert!(!tabs.select_index(3));
        assert!(tabs.select_index(1));
        assert_eq!(tabs.active().query().as_str(), "b");
    }

    #[test]
    fn sync_drops_gone_files_but_keeps_the_active_query() {
        let mut tabs = three();
        tabs.sync(&[name("c"), name("d")], &name("a"), &store())
            .unwrap();
        assert_eq!(names(&tabs), ["a", "c", "d"]);
        assert_eq!(tabs.active().query().as_str(), "a");
        // Nothing listed still leaves the selected tab.
        tabs.sync(&[], &name("d"), &store()).unwrap();
        assert_eq!(names(&tabs), ["d"]);
    }

    #[tokio::test]
    async fn sync_keeps_a_running_tab() {
        use quill_drivers::{
            ConnectionPool, settings::SqliteSettings, sqlite::SqliteConnectionPool,
        };
        let settings = SqliteSettings {
            path: ":memory:".into(),
            params: Default::default(),
        };
        let pool = SqliteConnectionPool::connect(&settings).await.unwrap();
        let results = tempfile::tempdir().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut tabs = three();
        tabs.active_mut().run = Some(Run::start(
            std::sync::Arc::new(pool),
            "SELECT 1".into(),
            results.path(),
            None,
            tx,
        ));
        tabs.sync(&[name("c")], &name("c"), &store()).unwrap();
        assert_eq!(names(&tabs), ["b", "c"]);
    }

    #[test]
    fn remove_keeps_a_tab_selected_and_never_empties() {
        let mut tabs = three();
        tabs.select_index(2);
        assert!(tabs.remove(&name("c")));
        assert_eq!(tabs.active().query().as_str(), "b");
        assert!(tabs.remove(&name("a")));
        assert_eq!(tabs.active().query().as_str(), "b");
        assert!(!tabs.remove(&name("b")));
        assert_eq!(names(&tabs), ["b"]);
    }

    #[test]
    fn title_keeps_the_active_tab_in_view() {
        let mut tabs = three();
        let (line, hits) = tabs.title(80);
        assert_eq!(line.to_string(), " a │ b │ c ");
        assert_eq!(hits, [(0..3, 0), (4..7, 1), (8..11, 2)]);

        tabs.select_index(2);
        let (line, hits) = tabs.title(7);
        assert_eq!(line.to_string(), " b │ c ");
        assert_eq!(hits, [(0..3, 1), (4..7, 2)]);
    }
}
