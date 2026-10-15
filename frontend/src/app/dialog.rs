use leptos::prelude::*;

/// Native dialog backdrops target the dialog itself; its padding is still inside.
pub(super) fn dismiss_outside(
    event: leptos::ev::MouseEvent,
    dialog: NodeRef<leptos::html::Dialog>,
    busy: bool,
) {
    if busy || event.target() != event.current_target() {
        return;
    }
    if let Some(dialog) = dialog.get() {
        let bounds = dialog.get_bounding_client_rect();
        let (x, y) = (f64::from(event.client_x()), f64::from(event.client_y()));
        if x < bounds.left() || x > bounds.right() || y < bounds.top() || y > bounds.bottom() {
            dialog.close();
        }
    }
}
