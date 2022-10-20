use crate::{
    compositor::{Component, Compositor, Context, Event, EventResult},
    keymap::{KeyTrie, KeyTrieNode, Keymap},
};

use arc_swap::access::DynGuard;
use helix_core::{
    syntax::{self, HighlightEvent},
    Rope, Tendril, Transaction,
};
use helix_view::{
    apply_transaction, document::Mode, editor::Action, graphics::Rect, keyboard::KeyCode,
    theme::Style, Document, Editor, View,
};
use once_cell::sync::Lazy;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use tui::buffer::Buffer as Surface;

use super::EditorView;

const UNSUPPORTED_COMMANDS: Lazy<HashSet<&str>> = Lazy::new(|| {
    HashSet::from([
        "global_search",
        "global_refactor",
        // "command_mode",
        "file_picker",
        "file_picker_in_current_directory",
        "code_action",
        "buffer_picker",
        "jumplist_picker",
        "symbol_picker",
        "select_references_to_symbol_under_cursor",
        "workspace_symbol_picker",
        "diagnostics_picker",
        "workspace_diagnostics_picker",
        "last_picker",
        "goto_definition",
        "goto_type_definition",
        "goto_implementation",
        "goto_file",
        "goto_file_hsplit",
        "goto_file_vsplit",
        "goto_reference",
        "goto_window_top",
        "goto_window_center",
        "goto_window_bottom",
        "goto_last_accessed_file",
        "goto_last_modified_file",
        "goto_last_modification",
        "goto_line",
        "goto_last_line",
        "goto_first_diag",
        "goto_last_diag",
        "goto_next_diag",
        "goto_prev_diag",
        "goto_line_start",
        "goto_line_end",
        "goto_next_buffer",
        "goto_previous_buffer",
        "signature_help",
        "completion",
        "hover",
        "select_next_sibling",
        "select_prev_sibling",
        "jump_view_right",
        "jump_view_left",
        "jump_view_up",
        "jump_view_down",
        "swap_view_right",
        "swap_view_left",
        "swap_view_up",
        "swap_view_down",
        "transpose_view",
        "rotate_view",
        "hsplit",
        "hsplit_new",
        "vsplit",
        "vsplit_new",
        "wonly",
        "select_textobject_around",
        "select_textobject_inner",
        "goto_next_function",
        "goto_prev_function",
        "goto_next_class",
        "goto_prev_class",
        "goto_next_parameter",
        "goto_prev_parameter",
        "goto_next_comment",
        "goto_prev_comment",
        "goto_next_test",
        "goto_prev_test",
        "goto_next_paragraph",
        "goto_prev_paragraph",
        "dap_launch",
        "dap_toggle_breakpoint",
        "dap_continue",
        "dap_pause",
        "dap_step_in",
        "dap_step_out",
        "dap_next",
        "dap_variables",
        "dap_terminate",
        "dap_edit_condition",
        "dap_edit_log",
        "dap_switch_thread",
        "dap_switch_stack_frame",
        "dap_enable_exceptions",
        "dap_disable_exceptions",
        "shell_pipe",
        "shell_pipe_to",
        "shell_insert_output",
        "shell_append_output",
        "shell_keep_pipe",
        "suspend",
        "rename_symbol",
        "record_macro",
        "replay_macro",
        "command_palette",
    ])
});

pub struct RefactorView {
    matches: HashMap<PathBuf, Vec<(usize, String)>>,
    line_map: HashMap<(PathBuf, usize), usize>,
    keymap: DynGuard<HashMap<Mode, Keymap>>,
    sticky: Option<KeyTrieNode>,
    apply_prompt: bool,
}

impl RefactorView {
    pub fn new(
        matches: HashMap<PathBuf, Vec<(usize, String)>>,
        editor: &mut Editor,
        editor_view: &mut EditorView,
        language_id: Option<String>,
    ) -> Self {
        let keymap = editor_view.keymaps.map();
        let mut review = RefactorView {
            matches,
            keymap,
            sticky: None,
            line_map: HashMap::new(),
            apply_prompt: false,
        };
        let mut doc_text = Rope::new();

        let mut count = 0;
        for (key, value) in &review.matches {
            for (line, text) in value {
                doc_text.insert(doc_text.len_chars(), &text);
                doc_text.insert(doc_text.len_chars(), "\n");
                review.line_map.insert((key.clone(), *line), count);
                count += 1;
            }
        }
        doc_text.split_off(doc_text.len_chars().saturating_sub(1));
        let mut doc = Document::from(doc_text, None);
        if let Some(language_id) = language_id {
            doc.set_language_by_language_id(&language_id, editor.syn_loader.clone())
                .ok();
        };
        editor.new_file_from_document(Action::Replace, doc);
        let doc = doc_mut!(editor);
        let viewid = editor.tree.insert(View::new(doc.id(), vec![]));
        editor.tree.focus = viewid;
        doc.ensure_view_init(viewid);
        doc.reset_selection(viewid);

        review
    }

    fn apply_refactor(&self, editor: &mut Editor) -> (usize, usize) {
        let replace_text = doc!(editor).text().clone();
        let mut view = view!(editor).clone();
        let mut documents: usize = 0;
        let mut count: usize = 0;
        for (key, value) in &self.matches {
            let mut changes = Vec::<(usize, usize, String)>::new();
            for (line, text) in value {
                if let Some(re_line) = self.line_map.get(&(key.clone(), *line)) {
                    let mut replace = replace_text
                        .get_line(*re_line)
                        .unwrap_or("\n".into())
                        .to_string()
                        .clone();
                    replace = replace.strip_suffix("\n").unwrap_or(&replace).to_string();
                    if text != &replace {
                        changes.push((*line, text.chars().count(), replace));
                    }
                }
            }
            if !changes.is_empty() {
                if let Some(doc) = editor
                    .open(&key, Action::Load)
                    .ok()
                    .and_then(|id| editor.document_mut(id))
                {
                    documents += 1;
                    let mut applychanges = Vec::<(usize, usize, Option<Tendril>)>::new();
                    for (line, length, text) in changes {
                        if doc.text().len_lines() > line {
                            let start = doc.text().line_to_char(line);
                            applychanges.push((
                                start,
                                start + length,
                                Some(Tendril::from(text.to_string())),
                            ));
                            count += 1;
                        }
                    }
                    let transaction = Transaction::change(doc.text(), applychanges.into_iter());
                    apply_transaction(&transaction, doc, &mut view);
                }
            }
        }
        (documents, count)
    }

    fn render_view(&self, editor: &Editor, surface: &mut Surface) {
        let doc = doc!(editor);
        let view = view!(editor);
        let offset = view.offset;
        let mut area = view.area;

        self.render_doc_name(surface, &mut area, offset);
        let highlights =
            EditorView::doc_syntax_highlights(&doc, offset, area.height, &editor.theme);
        let highlights: Box<dyn Iterator<Item = HighlightEvent>> = Box::new(syntax::merge(
            highlights,
            EditorView::doc_selection_highlights(
                editor.mode(),
                &doc,
                &view,
                &editor.theme,
                &editor.config().cursor_shape,
            ),
        ));

        EditorView::render_text_highlights(
            &doc,
            offset,
            area,
            surface,
            &editor.theme,
            highlights,
            &editor.config(),
        );
    }

    fn render_doc_name(
        &self,
        surface: &mut Surface,
        area: &mut Rect,
        offset: helix_core::Position,
    ) {
        let mut start = 0;
        for (key, value) in &self.matches {
            for (line, _) in value {
                if start >= offset.row {
                    let text = key.display().to_string() + ":" + line.to_string().as_str();
                    surface.set_string_truncated(
                        area.x as u16,
                        area.y + start as u16,
                        &text,
                        15,
                        |_| Style::default().fg(helix_view::theme::Color::Magenta),
                        true,
                        true,
                    );
                }
                start += 1;
            }
        }
        area.x = 15;
    }

    #[inline]
    fn close(&self, editor: &mut Editor) -> EventResult {
        editor.close_document(doc!(editor).id(), true).ok();
        editor.autoinfo = None;
        EventResult::Consumed(Some(Box::new(|compositor: &mut Compositor, _cx| {
            compositor.pop();
        })))
    }
}

impl Component for RefactorView {
    fn handle_event(&mut self, event: &Event, cx: &mut Context) -> EventResult {
        let config = cx.editor.config();
        let (view, doc) = current!(cx.editor);
        view.ensure_cursor_in_view(&doc, config.scrolloff);
        match event {
            Event::Key(event) => match event.code {
                KeyCode::Esc => {
                    self.sticky = None;
                }
                _ => {
                    // Temp solution
                    if self.apply_prompt {
                        if let Some(char) = event.char() {
                            if char == 'y' || char == 'Y' {
                                let (documents, count) = self.apply_refactor(cx.editor);
                                let result = format!(
                                    "Refactored {} documents, {} lines changed.",
                                    documents, count
                                );
                                cx.editor.set_status(result);
                                return self.close(cx.editor);
                            }
                        }
                        cx.editor.set_status("Aborted");
                        self.apply_prompt = false;
                        return EventResult::Consumed(None);
                    }
                    let sticky = self.sticky.clone();
                    if let Some(key) = sticky.as_ref().and_then(|sticky| sticky.get(event)).or(self
                        .keymap
                        .get(&cx.editor.mode)
                        .and_then(|map| map.get(event)))
                    {
                        match key {
                            KeyTrie::Leaf(command) => {
                                if UNSUPPORTED_COMMANDS.contains(command.name()) {
                                    cx.editor
                                        .set_status("Command not supported in refactor view");
                                    return EventResult::Consumed(None);
                                } else if command.name() == "wclose" {
                                    return self.close(cx.editor);
                                // TODO: custom command mode
                                } else if command.name() == "command_mode" {
                                    cx.editor.set_status("Apply changes to documents? (y/n): ");
                                    self.apply_prompt = true;
                                    return EventResult::Consumed(None);
                                }
                                self.sticky = None;
                                cx.editor.autoinfo = None;
                            }
                            KeyTrie::Sequence(_) => (),
                            KeyTrie::Node(node) => {
                                self.sticky = Some(node.clone());
                                cx.editor.autoinfo = Some(node.infobox());
                                return EventResult::Consumed(None);
                            }
                        }
                    }
                }
            },
            _ => (),
        }

        EventResult::Ignored(None)
    }

    fn render(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        let view = view_mut!(cx.editor);
        let area = area.clip_bottom(1);
        view.area = area;
        surface.clear_with(area, cx.editor.theme.get("ui.background"));

        self.render_view(&cx.editor, surface);
        if cx.editor.config().auto_info {
            if let Some(mut info) = cx.editor.autoinfo.take() {
                info.render(area, surface, cx);
                cx.editor.autoinfo = Some(info)
            }
        }
    }
}
