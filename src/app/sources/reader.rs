//! Compressed lookahead is separate from the small decoded viewport window.
pub(super) use super::reader_cache::Reader;
use super::*;
pub(super) fn publish(w: &SetupWindow, b: &mut Browser) {
    let g = sources(w);
    let page = b.page;
    b.reader.decoded.retain(|i, _| i.abs_diff(page) <= 1);
    let mut offset = 0.;
    let values = b
        .reader
        .ratios
        .iter()
        .enumerate()
        .map(|(i, &ratio)| {
            let row = crate::ReaderPage {
                image: if b.reader.decoded.contains_key(&i) {
                    g.get_reader_pages()
                        .row_data(i)
                        .map(|r| r.image)
                        .unwrap_or_default()
                } else {
                    Default::default()
                },
                ratio,
                offset,
                number: (i + 1) as i32,
            };
            offset += ratio;
            row
        })
        .collect::<Vec<_>>();
    let model = g.get_reader_pages();
    if let Some(m) = model.as_any().downcast_ref::<VecModel<crate::ReaderPage>>() {
        for (i, row) in values.into_iter().enumerate() {
            if i < m.row_count() {
                if m.row_data(i).as_ref() != Some(&row) {
                    m.set_row_data(i, row);
                }
            } else {
                m.push(row);
            }
        }
        while m.row_count() > b.pages.len() {
            m.remove(m.row_count() - 1);
        }
    } else {
        g.set_reader_pages(ModelRc::new(VecModel::from(values)));
    }
    g.set_total_ratio(offset);
    g.set_page(page as i32);
    g.set_page_image(
        g.get_reader_pages()
            .row_data(page)
            .map(|r| r.image)
            .unwrap_or_default(),
    );
    g.set_loaded_pages(b.reader.compressed.len() as i32);
}
pub(super) fn next_task(b: &mut Browser) -> Option<Task> {
    if b.pages.is_empty() {
        return None;
    }
    let i = b.page;
    // Visible page first, then one decoded neighbour on each side. Others stay compressed.
    let wanted = [
        Some(i),
        i.checked_add(1).filter(|&n| n < b.pages.len()),
        i.checked_sub(1),
    ];
    for n in wanted.into_iter().flatten() {
        if !b.reader.decoded.contains_key(&n) && !b.reader.failed.contains(&n) {
            return Some(Task::Page(
                b.pages[n].clone(),
                b.referer.clone(),
                n,
                b.reader.compressed.get(&n).cloned(),
                true,
            ));
        }
    }
    b.reader.target(i, b.pages.len());
    for n in i + 1..b.reader.target {
        if !b.reader.fetched.contains(&n) && !b.reader.failed.contains(&n) {
            return Some(Task::Page(
                b.pages[n].clone(),
                b.referer.clone(),
                n,
                None,
                false,
            ));
        }
    }
    None
}
pub(super) fn schedule(w: &SetupWindow, jobs: &Jobs, browser: &Mutex<Browser>) {
    let task = next_task(&mut browser.lock().unwrap());
    if let Some(t) = task {
        submit(w, jobs, browser, t);
    }
}
