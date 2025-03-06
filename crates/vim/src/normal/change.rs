use crate::{
    motion::{self, Motion},
    object::Object,
    state::Mode,
    Vim,
};
use editor::{
    display_map::{DisplaySnapshot, ToDisplayPoint},
    movement::TextLayoutDetails,
    scroll::Autoscroll,
    Bias, DisplayPoint,
};
use gpui::{Context, Window};
use language::Selection;

impl Vim {
    pub fn change_motion(
        &mut self,
        motion: Motion,
        times: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Some motions ignore failure when switching to normal mode
        let mut motion_succeeded = matches!(
            motion,
            Motion::Left
                | Motion::Right
                | Motion::EndOfLine { .. }
                | Motion::WrappingLeft
                | Motion::StartOfLine { .. }
        );
        self.update_editor(window, cx, |vim, editor, window, cx| {
            let text_layout_details = editor.text_layout_details(window);
            editor.transact(window, cx, |editor, window, cx| {
                // We are swapping to insert mode anyway. Just set the line end clipping behavior now
                editor.set_clip_at_line_ends(false, cx);
                editor.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                    s.move_with(|map, selection| {
                        motion_succeeded |= match motion {
                            Motion::NextWordStart { ignore_punctuation }
                            | Motion::NextSubwordStart { ignore_punctuation } => {
                                expand_changed_word_selection(
                                    map,
                                    selection,
                                    times,
                                    ignore_punctuation,
                                    &text_layout_details,
                                    motion == Motion::NextSubwordStart { ignore_punctuation },
                                )
                            }
                            _ => {
                                let result = motion.expand_selection(
                                    map,
                                    selection,
                                    times,
                                    false,
                                    &text_layout_details,
                                );
                                if let Motion::CurrentLine = motion {
                                    let mut start_offset =
                                        selection.start.to_offset(map, Bias::Left);
                                    let classifier = map
                                        .buffer_snapshot
                                        .char_classifier_at(selection.start.to_point(map));
                                    for (ch, offset) in map.buffer_chars_at(start_offset) {
                                        if ch == '\n' || !classifier.is_whitespace(ch) {
                                            break;
                                        }
                                        start_offset = offset + ch.len_utf8();
                                    }
                                    selection.start = start_offset.to_display_point(map);
                                }
                                result
                            }
                        }
                    });
                });
                vim.copy_selections_content(editor, motion.linewise(), cx);
                editor.insert("", window, cx);
                editor.refresh_inline_completion(true, false, window, cx);
            });
        });

        if motion_succeeded {
            self.switch_mode(Mode::Insert, false, window, cx)
        } else {
            self.switch_mode(Mode::Normal, false, window, cx)
        }
    }

    pub fn change_object(
        &mut self,
        object: Object,
        around: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut objects_found = false;

        self.update_editor(window, cx, |vim, editor, window, cx| {
            // We are swapping to insert mode anyway. Just set the line end clipping behavior now
            editor.set_clip_at_line_ends(false, cx);
            editor.transact(window, cx, |editor, window, cx| {
                editor.change_selections(Some(Autoscroll::fit()), window, cx, |s| {
                    s.move_with(|map, selection| {
                        objects_found |= object.expand_selection(map, selection, around, false);
                    });
                });
                if objects_found {
                    vim.copy_selections_content(editor, false, cx);
                    editor.insert("", window, cx);
                    editor.refresh_inline_completion(true, false, window, cx);
                }
            });
        });
        if objects_found {
            self.switch_mode(Mode::Insert, false, window, cx);
        } else {
            self.switch_mode(Mode::Normal, false, window, cx);
        }
    }
}

// From the docs https://vimdoc.sourceforge.net/htmldoc/motion.html
// Special case: "cw" and "cW" are treated like "ce" and "cE" if the cursor is
// on a non-blank.  This is because "cw" is interpreted as change-word, and a
// word does not include the following white space.  {Vi: "cw" when on a blank
// followed by other blanks changes only the first blank; this is probably a
// bug, because "dw" deletes all the blanks}
fn expand_changed_word_selection(
    map: &DisplaySnapshot,
    selection: &mut Selection<DisplayPoint>,
    times: Option<usize>,
    ignore_punctuation: bool,
    text_layout_details: &TextLayoutDetails,
    use_subword: bool,
) -> bool {
    let is_in_word = || {
        let classifier = map
            .buffer_snapshot
            .char_classifier_at(selection.start.to_point(map));
        let in_word = map
            .buffer_chars_at(selection.head().to_offset(map, Bias::Left))
            .next()
            .map(|(c, _)| !classifier.is_whitespace(c))
            .unwrap_or_default();
        in_word
    };
    if (times.is_none() || times.unwrap() == 1) && is_in_word() {
        let next_char = map
            .buffer_chars_at(
                motion::next_char(map, selection.end, false).to_offset(map, Bias::Left),
            )
            .next();
        match next_char {
            Some((' ', _)) => selection.end = motion::next_char(map, selection.end, false),
            _ => {
                if use_subword {
                    selection.end =
                        motion::next_subword_end(map, selection.end, ignore_punctuation, 1, false);
                } else {
                    selection.end =
                        motion::next_word_end(map, selection.end, ignore_punctuation, 1, false);
                }
                selection.end = motion::next_char(map, selection.end, false);
            }
        }
        true
    } else {
        let motion = if use_subword {
            Motion::NextSubwordStart { ignore_punctuation }
        } else {
            Motion::NextWordStart { ignore_punctuation }
        };
        motion.expand_selection(map, selection, times, false, text_layout_details)
    }
}

#[cfg(test)]
mod test {
    use indoc::indoc;

    use crate::test::NeovimBackedTestContext;

    #[gpui::test]
    async fn test_change_h(cx: &mut gpui::TestAppContext) {
        let mut cx = NeovimBackedTestContext::new(cx).await;
        cx.simulate("c h", "Teˇst").await.assert_matches();
        cx.simulate("c h", "Tˇest").await.assert_matches();
        cx.simulate("c h", "ˇTest").await.assert_matches();
        cx.simulate(
            "c h",
            indoc! {"
            Test
            ˇtest"},
        )
        .await
        .assert_matches();
    }

    #[gpui::test]
    async fn test_change_backspace(cx: &mut gpui::TestAppContext) {
        let mut cx = NeovimBackedTestContext::new(cx).await;
        cx.simulate("c backspace", "Teˇst").await.assert_matches();
        cx.simulate("c backspace", "Tˇest").await.assert_matches();
        cx.simulate("c backspace", "ˇTest").await.assert_matches();
        cx.simulate(
            "c backspace",
            indoc! {"
            Test
            ˇtest"},
        )
        .await
        .assert_matches();
    }

    #[gpui::test]
    async fn test_change_l(cx: &mut gpui::TestAppContext) {
        let mut cx = NeovimBackedTestContext::new(cx).await;
        cx.simulate("c l", "Teˇst").await.assert_matches();
        cx.simulate("c l", "Tesˇt").await.assert_matches();
    }

    #[gpui::test]
    async fn test_change_w(cx: &mut gpui::TestAppContext) {
        let mut cx = NeovimBackedTestContext::new(cx).await;
        cx.simulate("c w", "Teˇst").await.assert_matches();
        cx.simulate("c w", "Tˇest test").await.assert_matches();
        cx.simulate("c w", "Testˇ  test").await.assert_matches();
        cx.simulate("c w", "Tesˇt  test").await.assert_matches();
        cx.simulate(
            "c w",
            indoc! {"
                Test teˇst
                test"},
        )
        .await
        .assert_matches();
        cx.simulate(
            "c w",
            indoc! {"
                Test tesˇt
                test"},
        )
        .await
        .assert_matches();
        cx.simulate(
            "c w",
            indoc! {"
                Test test
                ˇ
                test"},
        )
        .await
        .assert_matches();

        cx.simulate("c shift-w", "Test teˇst-test test")
            .await
            .assert_matches();
    }

    #[gpui::test]
    async fn test_change_e(cx: &mut gpui::TestAppContext) {
        let mut cx = NeovimBackedTestContext::new(cx).await;
        cx.simulate("c e", "Teˇst Test").await.assert_matches();
        cx.simulate("c e", "Tˇest test").await.assert_matches();
        cx.simulate(
            "c e",
            indoc! {"
                Test teˇst
                test"},
        )
        .await
        .assert_matches();
        cx.simulate(
            "c e",
            indoc! {"
                Test tesˇt
                test"},
        )
        .await
        .assert_matches();
        cx.simulate(
            "c e",
            indoc! {"
                Test test
                ˇ
                test"},
        )
        .await
        .assert_matches();

        cx.simulate("c shift-e", "Test teˇst-test test")
            .await
            .assert_matches();
    }

    #[gpui::test]
    async fn test_change_b(cx: &mut gpui::TestAppContext) {
        let mut cx = NeovimBackedTestContext::new(cx).await;
        cx.simulate("c b", "Teˇst Test").await.assert_matches();
        cx.simulate("c b", "Test ˇtest").await.assert_matches();
        cx.simulate("c b", "Test1 test2 ˇtest3")
            .await
            .assert_matches();
        cx.simulate(
            "c b",
            indoc! {"
                Test test
                ˇtest"},
        )
        .await
        .assert_matches();
        cx.simulate(
            "c b",
            indoc! {"
                Test test
                ˇ
                test"},
        )
        .await
        .assert_matches();

        cx.simulate("c shift-b", "Test test-test ˇtest")
            .await
            .assert_matches();
    }

    #[gpui::test]
    async fn test_change_end_of_line(cx: &mut gpui::TestAppContext) {
        let mut cx = NeovimBackedTestContext::new(cx).await;
        cx.simulate(
            "c $",
            indoc! {"
            The qˇuick
            brown fox"},
        )
        .await
        .assert_matches();
        cx.simulate(
            "c $",
            indoc! {"
            The quick
            ˇ
            brown fox"},
        )
        .await
        .assert_matches();
    }

    #[gpui::test]
    async fn test_change_0(cx: &mut gpui::TestAppContext) {
        let mut cx = NeovimBackedTestContext::new(cx).await;

        cx.simulate(
            "c 0",
            indoc! {"
            The qˇuick
            brown fox"},
        )
        .await
        .assert_matches();
        cx.simulate(
            "c 0",
            indoc! {"
            The quick
            ˇ
            brown fox"},
        )
        .await
        .assert_matches();
    }

    #[gpui::test]
    async fn test_change_k(cx: &mut gpui::TestAppContext) {
        let mut cx = NeovimBackedTestContext::new(cx).await;

        cx.simulate(
            "c k",
            indoc! {"
            The quick
            brown ˇfox
            jumps over"},
        )
        .await
        .assert_matches();
        cx.simulate(
            "c k",
            indoc! {"
            The quick
            brown fox
            jumps ˇover"},
        )
        .await
        .assert_matches();
        cx.simulate(
            "c k",
            indoc! {"
            The qˇuick
            brown fox
            jumps over"},
        )
        .await
        .assert_matches();
        cx.simulate(
            "c k",
            indoc! {"
            ˇ
            brown fox
            jumps over"},
        )
        .await
        .assert_matches();
    }

    #[gpui::test]
    async fn test_change_j(cx: &mut gpui::TestAppContext) {
        let mut cx = NeovimBackedTestContext::new(cx).await;
        cx.simulate(
            "c j",
            indoc! {"
            The quick
            brown ˇfox
            jumps over"},
        )
        .await
        .assert_matches();
        cx.simulate(
            "c j",
            indoc! {"
            The quick
            brown fox
            jumps ˇover"},
        )
        .await
        .assert_matches();
        cx.simulate(
            "c j",
            indoc! {"
            The qˇuick
            brown fox
            jumps over"},
        )
        .await
        .assert_matches();
        cx.simulate(
            "c j",
            indoc! {"
            The quick
            brown fox
            ˇ"},
        )
        .await
        .assert_matches();
    }

    #[gpui::test]
    async fn test_change_end_of_document(cx: &mut gpui::TestAppContext) {
        let mut cx = NeovimBackedTestContext::new(cx).await;
        cx.simulate(
            "c shift-g",
            indoc! {"
            The quick
            brownˇ fox
            jumps over
            the lazy"},
        )
        .await
        .assert_matches();
        cx.simulate(
            "c shift-g",
            indoc! {"
            The quick
            brownˇ fox
            jumps over
            the lazy"},
        )
        .await
        .assert_matches();
        cx.simulate(
            "c shift-g",
            indoc! {"
            The quick
            brown fox
            jumps over
            the lˇazy"},
        )
        .await
        .assert_matches();
        cx.simulate(
            "c shift-g",
            indoc! {"
            The quick
            brown fox
            jumps over
            ˇ"},
        )
        .await
        .assert_matches();
    }

    #[gpui::test]
    async fn test_change_cc(cx: &mut gpui::TestAppContext) {
        let mut cx = NeovimBackedTestContext::new(cx).await;
        cx.simulate(
            "c c",
            indoc! {"
           The quick
             brownˇ fox
           jumps over
           the lazy"},
        )
        .await
        .assert_matches();

        cx.simulate(
            "c c",
            indoc! {"
           ˇThe quick
           brown fox
           jumps over
           the lazy"},
        )
        .await
        .assert_matches();

        cx.simulate(
            "c c",
            indoc! {"
           The quick
             broˇwn fox
           jumps over
           the lazy"},
        )
        .await
        .assert_matches();
    }

    #[gpui::test]
    async fn test_change_gg(cx: &mut gpui::TestAppContext) {
        let mut cx = NeovimBackedTestContext::new(cx).await;
        cx.simulate(
            "c g g",
            indoc! {"
            The quick
            brownˇ fox
            jumps over
            the lazy"},
        )
        .await
        .assert_matches();
        cx.simulate(
            "c g g",
            indoc! {"
            The quick
            brown fox
            jumps over
            the lˇazy"},
        )
        .await
        .assert_matches();
        cx.simulate(
            "c g g",
            indoc! {"
            The qˇuick
            brown fox
            jumps over
            the lazy"},
        )
        .await
        .assert_matches();
        cx.simulate(
            "c g g",
            indoc! {"
            ˇ
            brown fox
            jumps over
            the lazy"},
        )
        .await
        .assert_matches();
    }

    #[gpui::test]
    async fn test_repeated_cj(cx: &mut gpui::TestAppContext) {
        let mut cx = NeovimBackedTestContext::new(cx).await;

        for count in 1..=5 {
            cx.simulate_at_each_offset(
                &format!("c {count} j"),
                indoc! {"
                    ˇThe quˇickˇ browˇn
                    ˇ
                    ˇfox ˇjumpsˇ-ˇoˇver
                    ˇthe lazy dog
                    "},
            )
            .await
            .assert_matches();
        }
    }

    #[gpui::test]
    async fn test_repeated_cl(cx: &mut gpui::TestAppContext) {
        let mut cx = NeovimBackedTestContext::new(cx).await;

        for count in 1..=5 {
            cx.simulate_at_each_offset(
                &format!("c {count} l"),
                indoc! {"
                    ˇThe quˇickˇ browˇn
                    ˇ
                    ˇfox ˇjumpsˇ-ˇoˇver
                    ˇthe lazy dog
                    "},
            )
            .await
            .assert_matches();
        }
    }

    #[gpui::test]
    async fn test_repeated_cb(cx: &mut gpui::TestAppContext) {
        let mut cx = NeovimBackedTestContext::new(cx).await;

        for count in 1..=5 {
            cx.simulate_at_each_offset(
                &format!("c {count} b"),
                indoc! {"
                ˇThe quˇickˇ browˇn
                ˇ
                ˇfox ˇjumpsˇ-ˇoˇver
                ˇthe lazy dog
                "},
            )
            .await
            .assert_matches()
        }
    }

    #[gpui::test]
    async fn test_repeated_ce(cx: &mut gpui::TestAppContext) {
        let mut cx = NeovimBackedTestContext::new(cx).await;

        for count in 1..=5 {
            cx.simulate_at_each_offset(
                &format!("c {count} e"),
                indoc! {"
                    ˇThe quˇickˇ browˇn
                    ˇ
                    ˇfox ˇjumpsˇ-ˇoˇver
                    ˇthe lazy dog
                    "},
            )
            .await
            .assert_matches();
        }
    }
}
