use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use gtk::glib;
use gtk::prelude::*;

use rufin_core::settings::layout::LibraryField;

mod header_cell_imp {
    use gtk::prelude::*;
    use gtk::{CompositeTemplate, TemplateChild, glib, subclass::prelude::*};

    #[derive(CompositeTemplate, Default)]
    #[template(resource = "/io/github/screwys/Rufin/ui/routes/column_view_header_cell.ui")]
    pub struct HeaderCell {
        #[template_child]
        pub content: TemplateChild<gtk::Box>,
        #[template_child]
        pub label: TemplateChild<gtk::Label>,
        #[template_child]
        pub icon: TemplateChild<gtk::Image>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HeaderCell {
        const NAME: &'static str = "RufinColumnViewHeaderCell";
        type Type = super::HeaderCell;
        type ParentType = gtk::Widget;

        fn class_init(class: &mut Self::Class) {
            class.bind_template();
        }

        fn instance_init(instance: &glib::subclass::InitializingObject<Self>) {
            instance.init_template();
        }
    }

    impl ObjectImpl for HeaderCell {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
            self.dispose_template();
        }
    }

    impl WidgetImpl for HeaderCell {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            if orientation == gtk::Orientation::Horizontal {
                // The column's fixed width owns horizontal sizing, including clipped labels.
                (0, 0, -1, -1)
            } else {
                self.content.measure(orientation, for_size)
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.content.allocate(width, height, baseline, None);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().snapshot_child(&*self.content, snapshot);
        }
    }
}

glib::wrapper! {
    pub struct HeaderCell(ObjectSubclass<header_cell_imp::HeaderCell>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl HeaderCell {
    pub fn new(title: &str, field: Option<LibraryField>, position: usize) -> Self {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        let cell: Self = glib::Object::new();
        let imp = cell.imp();
        cell.update_property(&[gtk::accessible::Property::Label(title)]);
        cell.update_relation(&[gtk::accessible::Relation::ColIndex(position as i32 + 1)]);
        imp.label.set_label(title);
        imp.icon.set_tooltip_text(Some(title));
        imp.label
            .set_xalign(if field == Some(LibraryField::RowIndex) {
                0.5
            } else {
                0.0
            });
        let icon = match field {
            Some(LibraryField::Duration) => Some("rufin-preferences-system-time-symbolic"),
            Some(LibraryField::Favorite) => Some(crate::favorites::FAVORITE_ADD_ICON),
            _ => None,
        };
        imp.label.set_visible(icon.is_none());
        imp.icon
            .set_halign(if field == Some(LibraryField::Favorite) {
                gtk::Align::Center
            } else {
                gtk::Align::Start
            });
        imp.icon.set_icon_name(icon);
        imp.icon.set_visible(icon.is_some());
        cell
    }
}

#[derive(Clone)]
pub struct ColumnViewHeader {
    root: gtk::Box,
    bindings: Rc<RefCell<Vec<glib::Binding>>>,
    on_click: Rc<RefCell<Option<Rc<dyn Fn(LibraryField)>>>>,
    arrows: Rc<RefCell<Vec<(LibraryField, gtk::Image)>>>,
    sort: Rc<Cell<Option<(LibraryField, bool)>>>,
    default_sort: Rc<Cell<Option<(LibraryField, bool)>>>,
}

impl Default for ColumnViewHeader {
    fn default() -> Self {
        Self::new()
    }
}

impl ColumnViewHeader {
    pub fn new() -> Self {
        let resource = crate::ui_resource::COLUMN_VIEW_HEADER_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        let root = crate::ui_resource::object(&builder, resource, "header");
        Self {
            root,
            bindings: Rc::default(),
            on_click: Rc::default(),
            arrows: Rc::default(),
            sort: Rc::default(),
            default_sort: Rc::default(),
        }
    }

    /// The list's default order; it gets no arrow.
    pub fn set_default_sort(&self, sort: (LibraryField, bool)) {
        self.default_sort.set(Some(sort));
    }

    /// Shows a direction arrow on the header of the sorted column.
    pub fn set_sort(&self, sort: Option<(LibraryField, bool)>) {
        self.sort.set(sort);
        let sort = sort.filter(|sort| Some(*sort) != self.default_sort.get());
        for (field, arrow) in self.arrows.borrow().iter() {
            // The merged title column sorts by Title and "#" by the default key.
            let field = match *field {
                LibraryField::TitleMerged => LibraryField::Title,
                LibraryField::RowIndex => self.default_sort.get().map_or(*field, |sort| sort.0),
                field => field,
            };
            let active = sort.filter(|(sorted, _)| *sorted == field);
            arrow.set_visible(active.is_some());
            if let Some((_, descending)) = active {
                arrow.set_icon_name(Some(crate::controls::sort_order_icon(descending)));
            }
        }
    }

    /// Called with the column's field when its header cell is clicked.
    pub fn connect_clicked(&self, callback: impl Fn(LibraryField) + 'static) {
        self.on_click.replace(Some(Rc::new(callback)));
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    pub fn replace(&self, columns: &[(gtk::ColumnViewColumn, Option<LibraryField>)]) {
        for binding in self.bindings.take() {
            binding.unbind();
        }
        while let Some(child) = self.root.first_child() {
            self.root.remove(&child);
        }
        let mut bindings = Vec::with_capacity(columns.len() * 2);
        let mut arrows = Vec::new();
        for (position, (column, field)) in columns.iter().enumerate() {
            let title = column.title().unwrap_or_default();
            let cell = HeaderCell::new(&title, *field, position);
            bindings.push(
                column
                    .bind_property("fixed-width", &cell, "width-request")
                    .sync_create()
                    .build(),
            );
            bindings.push(
                column
                    .bind_property("visible", &cell, "visible")
                    .sync_create()
                    .build(),
            );
            if let Some(field) = *field {
                let on_click = Rc::clone(&self.on_click);
                let click = gtk::GestureClick::new();
                click.connect_released(move |_, _, _, _| {
                    if let Some(callback) = on_click.borrow().clone() {
                        callback(field);
                    }
                });
                cell.add_controller(click);
                let arrow = gtk::Image::new();
                arrow.set_visible(false);
                arrow.set_pixel_size(12);
                arrow.add_css_class("dim-label");
                gtk::subclass::prelude::ObjectSubclassIsExt::imp(&cell)
                    .content
                    .append(&arrow);
                arrows.push((field, arrow));
                cell.add_css_class("sortable-header");
                cell.set_cursor_from_name(Some("pointer"));
            }
            self.root.append(&cell);
        }
        self.bindings.replace(bindings);
        self.arrows.replace(arrows);
        self.set_sort(self.sort.get());
    }

    pub fn set_visible(&self, visible: bool) {
        self.root.set_visible(visible);
    }
}

#[derive(Clone)]
pub struct TableFrame {
    root: gtk::Box,
    header_host: gtk::Box,
}

impl TableFrame {
    pub fn new(content: &impl IsA<gtk::Widget>, header: Option<&gtk::Widget>) -> Self {
        let resource = crate::ui_resource::TABLE_FRAME_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
            root: gtk::Box,
            header_host: gtk::Box,
            content_host: gtk::Box,
        });
        content_host.append(content);
        let frame = Self { root, header_host };
        frame.set_header(header);
        frame
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    pub fn set_header(&self, header: Option<&gtk::Widget>) {
        while let Some(child) = self.header_host.first_child() {
            self.header_host.remove(&child);
        }
        if let Some(header) = header {
            self.header_host.append(header);
        }
        self.header_host.set_visible(header.is_some());
    }
}
