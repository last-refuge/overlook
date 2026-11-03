use overlook_core::toolbar::{
    ToolbarAction as A, ToolbarArea as Area, ToolbarLayouts, ToolbarModule as M,
};
use overlook_store::Store;

#[test]
fn toolbar_order_and_visibility_survive_reopening_the_profile() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("overlook.sqlite");
    let mut preferences = ToolbarLayouts::default();
    let mut mail = preferences.layout(M::Mail);
    mail.move_action(A::Archive, Area::More, None);
    mail.move_action(A::Forward, Area::Strip, Some(A::NewMessage));
    preferences.set(M::Mail, mail.clone());
    let mut calendar = preferences.layout(M::Calendar);
    calendar.move_action(A::Agenda, Area::Strip, Some(A::NewEvent));
    preferences.set(M::Calendar, calendar.clone());
    {
        let store = Store::open(&path).unwrap();
        store.set_setting("toolbar.layouts", &preferences).unwrap();
    }
    let store = Store::open(&path).unwrap();
    let loaded: ToolbarLayouts = store.setting("toolbar.layouts").unwrap().unwrap();
    assert_eq!(loaded.layout(M::Mail), mail);
    assert_eq!(loaded.layout(M::Calendar), calendar);
    assert_eq!(loaded.layout(M::Contacts), M::Contacts.default_layout());
}
