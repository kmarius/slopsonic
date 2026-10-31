use super::icons::*;
use adw::prelude::*;
use gtk::glib;
use gtk_widgets::{
    artwork::ArtworkTile,
    detail_links::DetailLinkBinding,
    favorites::{favorite_icon_button, set_favorite_button_active},
    layout::{AllocationOwner, allocation_owner},
    localization::{bind_widget_accessible_label, bind_widget_tooltip},
    ratings::RatingControl,
    scale::{
        horizontal_scale_value_at_x, install_pointer_value_bubble, install_scale_fill_updates,
        install_value_bubble_dismissal, point_value_bubble_at_slider,
    },
};
use playback::{PlaybackView, RepeatMode, TransportStatus};
use std::time::Duration;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
pub const BOTTOM_PLAYER_HEIGHT: i32 = 90;
pub const BOTTOM_PLAYER_COVER_START_INSET: i32 = 14;
pub const BOTTOM_PLAYER_COVER_SIZE: i32 = 60;
pub const BOTTOM_PLAYER_EDGE_PADDING: i32 = 8;
pub const BOTTOM_PLAYER_HORIZONTAL_PADDING: i32 =
    BOTTOM_PLAYER_COVER_START_INSET - BOTTOM_PLAYER_EDGE_PADDING;
pub const BOTTOM_PLAYER_NOW_PLAYING_SPACING: i32 = 8;
// Six side buttons and the play button, with the same gap between all of them.
pub const BOTTOM_PLAYER_TRANSPORT_WIDTH: i32 = BOTTOM_PLAYER_SIDE_BUTTON_SIZE * 6
    + BOTTOM_PLAYER_PLAY_BUTTON_SIZE
    + BOTTOM_PLAYER_BUTTON_GAP * 6;
pub const BOTTOM_PLAYER_PROGRESS_WIDTH: i32 = 320;
pub const BOTTOM_PLAYER_PROGRESS_MIN_WIDTH: i32 = 140;
pub const BOTTOM_PLAYER_BUTTON_ROW_HEIGHT: i32 = 58;
pub const BOTTOM_PLAYER_SIDE_BUTTON_SIZE: i32 = 36;
pub const BOTTOM_PLAYER_PLAY_BUTTON_SIZE: i32 = 44;
// Keeps the refill ring, 4px outside the play button, inside the button row.
pub const BOTTOM_PLAYER_BUTTON_OFFSET_Y: f64 = 3.0;
pub const BOTTOM_PLAYER_BUTTON_GAP: i32 = 8;
pub const BOTTOM_PLAYER_PROGRESS_HEIGHT: i32 = 24;
pub const BOTTOM_PLAYER_ACTION_BUTTON_SIZE: i32 = 32;
pub const BOTTOM_PLAYER_TITLE_MENU_BUTTON_SIZE: i32 = 18;
pub const BOTTOM_PLAYER_TITLE_ROW_HEIGHT: i32 = 20;
pub const BOTTOM_PLAYER_META_ROW_HEIGHT: i32 = 18;
pub const BOTTOM_PLAYER_ACTION_SPACING: i32 = 2;
pub const BOTTOM_PLAYER_ACTION_ROW_OFFSET_Y: i32 = 5;
pub const BOTTOM_PLAYER_RATING_WIDTH: i32 = 85;
pub const BOTTOM_PLAYER_RATING_HEIGHT: i32 = 24;
pub const BOTTOM_PLAYER_PROGRESS_SPACING: i32 = 6;
pub const BOTTOM_PLAYER_VOLUME_SPACING: i32 = BOTTOM_PLAYER_ACTION_SPACING;
pub const BOTTOM_PLAYER_VOLUME_SLOT_WIDTH: i32 = 95;
pub const BOTTOM_PLAYER_VOLUME_SLOT_LAYOUT_WIDTH: i32 = 90;
pub const BOTTOM_PLAYER_TINY_TRANSPORT_WIDTH: i32 = BOTTOM_PLAYER_SIDE_BUTTON_SIZE * 2
    + BOTTOM_PLAYER_PLAY_BUTTON_SIZE
    + BOTTOM_PLAYER_BUTTON_GAP * 2;
pub const BOTTOM_PLAYER_IDENTITY_MIN_WIDTH: i32 = 85;
pub const BOTTOM_PLAYER_IDENTITY_COMPACT_MIN_WIDTH: i32 = BOTTOM_PLAYER_EDGE_PADDING * 2
    + BOTTOM_PLAYER_TRANSPORT_WIDTH
    + (BOTTOM_PLAYER_HORIZONTAL_PADDING
        + BOTTOM_PLAYER_COVER_SIZE
        + BOTTOM_PLAYER_NOW_PLAYING_SPACING
        + BOTTOM_PLAYER_IDENTITY_MIN_WIDTH)
        * 2;
pub const BOTTOM_PLAYER_VOLUME_GROUP_MIN_WIDTH: i32 = BOTTOM_PLAYER_ACTION_BUTTON_SIZE * 2
    + BOTTOM_PLAYER_VOLUME_SPACING * 2
    + BOTTOM_PLAYER_VOLUME_SLOT_LAYOUT_WIDTH;
pub const BOTTOM_PLAYER_ACTIONS_COMPACT_MIN_WIDTH: i32 = BOTTOM_PLAYER_EDGE_PADDING * 2
    + BOTTOM_PLAYER_TRANSPORT_WIDTH
    + BOTTOM_PLAYER_VOLUME_GROUP_MIN_WIDTH * 2;
pub const BOTTOM_PLAYER_COMPACT_MIN_WIDTH: i32 =
    if BOTTOM_PLAYER_IDENTITY_COMPACT_MIN_WIDTH > BOTTOM_PLAYER_ACTIONS_COMPACT_MIN_WIDTH {
        BOTTOM_PLAYER_IDENTITY_COMPACT_MIN_WIDTH
    } else {
        BOTTOM_PLAYER_ACTIONS_COMPACT_MIN_WIDTH
    };
pub const BOTTOM_PLAYER_TINY_WIDTH: i32 = BOTTOM_PLAYER_COMPACT_MIN_WIDTH;
const BOTTOM_PLAYER_PROGRESS_CENTER_PERCENT: i32 = 95;
const BOTTOM_PLAYER_LAYOUT_COLUMNS: i32 = 3;
pub const VOLUME_PERSIST_DELAY: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BottomPlayerActions {
    Volume,
    Favorite,
    Queue,
}

pub struct PlayerControls {
    pub root: AllocationOwner,
    pub cover: ArtworkTile,
    pub title: gtk::Label,
    pub menu_button: gtk::Button,
    pub artist: gtk::Label,
    pub artist_links: RefCell<Option<DetailLinkBinding>>,
    pub album: gtk::Label,
    pub album_links: RefCell<Option<DetailLinkBinding>>,
    pub now_playing_wall: gtk::Box,
    pub right_slot: gtk::Overlay,
    pub transport: gtk::Box,
    pub transport_slot: gtk::Box,
    pub transport_buttons: gtk::Fixed,
    pub random_button: gtk::Button,
    pub previous_button: gtk::Button,
    pub play_button: gtk::Button,
    pub play_icon: gtk::Image,
    pub play_ring: crate::refill_ring::RefillRing,
    pub next_button: gtk::Button,
    pub shuffle_button: gtk::Button,
    pub repeat_button: gtk::Button,
    pub dj_button: gtk::Button,
    pub queue_button: gtk::Button,
    pub queue_icon: gtk::Image,
    pub favorite_button: gtk::Button,
    pub rating: RatingControl,
    pub progress_row: gtk::Box,
    pub elapsed: gtk::Label,
    pub progress: gtk::Scale,
    pub duration: gtk::Label,
    pub timestamp_size_group: gtk::SizeGroup,
    pub actions: gtk::Overlay,
    pub action_buttons: gtk::Box,
    pub mute_button: gtk::Button,
    pub mute_icon: gtk::Image,
    pub mute_icon_state: Rc<Cell<VolumeIcon>>,
    pub volume: gtk::Scale,
    pub settings_button: gtk::MenuButton,
}

pub struct NowPlayingControls {
    pub root: gtk::Box,
    pub cover: ArtworkTile,
    pub title: gtk::Label,
    pub menu_button: gtk::Button,
    pub artist: gtk::Label,
    pub album: gtk::Label,
}

pub struct TransportControls {
    pub root: gtk::Box,
    pub buttons: gtk::Fixed,
    pub random_button: gtk::Button,
    pub previous_button: gtk::Button,
    pub play_button: gtk::Button,
    pub play_icon: gtk::Image,
    pub play_ring: crate::refill_ring::RefillRing,
    pub next_button: gtk::Button,
    pub shuffle_button: gtk::Button,
    pub repeat_button: gtk::Button,
    pub dj_button: gtk::Button,
    pub progress_row: gtk::Box,
    pub elapsed: gtk::Label,
    pub progress: gtk::Scale,
    pub duration: gtk::Label,
    pub timestamp_size_group: gtk::SizeGroup,
}

pub struct PlayerActionControls {
    pub root: gtk::Overlay,
    pub buttons: gtk::Box,
    pub queue_button: gtk::Button,
    pub queue_icon: gtk::Image,
    pub favorite_button: gtk::Button,
    pub rating: RatingControl,
    pub mute_button: gtk::Button,
    pub mute_icon: gtk::Image,
    pub mute_icon_state: Rc<Cell<VolumeIcon>>,
    pub volume: gtk::Scale,
    pub settings_button: gtk::MenuButton,
}

pub fn build_bottom_player() -> PlayerControls {
    let resource = crate::ui_resource::BOTTOM_PLAYER_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        root: gtk::Box,
        now_playing_wall: gtk::Box,
        transport_slot: gtk::Box,
    });
    root.set_height_request(BOTTOM_PLAYER_HEIGHT);
    transport_slot.set_width_request(BOTTOM_PLAYER_TRANSPORT_WIDTH);

    let NowPlayingControls {
        root: now_playing,
        cover,
        title,
        menu_button,
        artist,
        album,
    } = build_now_playing_controls(&builder, resource);

    let TransportControls {
        root: transport,
        buttons: transport_buttons,
        random_button,
        previous_button,
        play_button,
        play_icon,
        play_ring,
        next_button,
        shuffle_button,
        repeat_button,
        dj_button,
        progress_row,
        elapsed,
        progress,
        duration,
        timestamp_size_group,
    } = build_transport_controls(&builder, resource);

    now_playing_wall.append(&now_playing);

    let PlayerActionControls {
        root: actions,
        buttons: action_buttons,
        queue_button,
        queue_icon,
        favorite_button,
        rating,
        mute_button,
        mute_icon,
        mute_icon_state,
        volume,
        settings_button,
    } = build_player_action_controls(&builder, resource);

    let left_slot = bottom_player_allocated_slot(&now_playing_wall, 1, 1);
    left_slot.set_hexpand(true);
    left_slot.set_halign(gtk::Align::Fill);
    left_slot.set_valign(gtk::Align::Fill);
    let right_slot = bottom_player_allocated_slot(&actions, 1, 1);
    right_slot.set_hexpand(true);
    right_slot.set_halign(gtk::Align::Fill);
    right_slot.set_valign(gtk::Align::Fill);

    transport_slot.append(&transport);

    root.append(&left_slot);
    root.append(&transport_slot);
    root.append(&right_slot);
    // Bottom-player topology follows the real allocation. Applying it during GTK's tentative
    // height-for-width measurement lets a provisional width and the allocated width alternate,
    // continuously invalidating the shell layout.
    let allocation_root = allocation_owner(&root, |_, _| {});

    PlayerControls {
        root: allocation_root,
        cover,
        title,
        menu_button,
        artist,
        artist_links: RefCell::new(None),
        album,
        album_links: RefCell::new(None),
        now_playing_wall,
        right_slot,
        transport,
        transport_slot,
        transport_buttons,
        random_button,
        previous_button,
        play_button,
        play_icon,
        play_ring,
        next_button,
        shuffle_button,
        repeat_button,
        dj_button,
        queue_button,
        queue_icon,
        favorite_button,
        rating,
        progress_row,
        elapsed,
        progress,
        duration,
        timestamp_size_group,
        actions,
        action_buttons,
        mute_button,
        mute_icon,
        mute_icon_state,
        volume,
        settings_button,
    }
}

pub fn build_now_playing_controls(builder: &gtk::Builder, resource: &str) -> NowPlayingControls {
    gtk_widgets::objects!(builder, resource, {
        now_playing: gtk::Box,
        identity: gtk::Box,
        title_row: gtk::Box,
        title_slide: gtk::ScrolledWindow, title_viewport: gtk::Viewport,
        artist_slide: gtk::ScrolledWindow, artist_viewport: gtk::Viewport,
        album_slide: gtk::ScrolledWindow, album_viewport: gtk::Viewport,
        title: gtk::Label,
        menu_button: gtk::Button,
        artist: gtk::Label,
        album: gtk::Label,
    });
    now_playing.set_spacing(BOTTOM_PLAYER_NOW_PLAYING_SPACING);
    now_playing.set_margin_start(BOTTOM_PLAYER_HORIZONTAL_PADDING);

    let cover = ArtworkTile::new(BOTTOM_PLAYER_COVER_SIZE);
    cover.area.set_valign(gtk::Align::Center);
    cover.area.set_cursor_from_name(Some("pointer"));
    let cover_label = "Open fullscreen player".to_string();
    cover.area.set_tooltip_text(Some(&cover_label));
    cover
        .area
        .update_property(&[gtk::accessible::Property::Label(&cover_label)]);
    now_playing.append(&cover.area);

    bind_widget_accessible_label(&menu_button, "More actions");
    menu_button.set_size_request(
        BOTTOM_PLAYER_TITLE_MENU_BUTTON_SIZE,
        BOTTOM_PLAYER_TITLE_MENU_BUTTON_SIZE,
    );
    title_row.set_height_request(BOTTOM_PLAYER_TITLE_ROW_HEIGHT);
    gtk_widgets::text_slide::install(&title_slide, &title_viewport, &title);
    gtk_widgets::text_slide::install(&artist_slide, &artist_viewport, &artist);
    gtk_widgets::text_slide::install(&album_slide, &album_viewport, &album);
    title.set_height_request(BOTTOM_PLAYER_TITLE_ROW_HEIGHT);
    artist.set_height_request(BOTTOM_PLAYER_META_ROW_HEIGHT);
    album.set_height_request(BOTTOM_PLAYER_META_ROW_HEIGHT);
    now_playing.append(&identity);

    NowPlayingControls {
        root: now_playing,
        cover,
        title,
        menu_button,
        artist,
        album,
    }
}

pub fn bottom_player_allocated_slot(
    child: &impl IsA<gtk::Widget>,
    width: i32,
    height: i32,
) -> gtk::Overlay {
    let slot = gtk::Overlay::new();
    let measure = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    measure.set_size_request(width, height);
    measure.set_accessible_role(gtk::AccessibleRole::Presentation);
    slot.set_child(Some(&measure));
    slot.add_overlay(child);
    slot.set_measure_overlay(child, false);
    slot.set_clip_overlay(child, true);
    slot
}

pub fn build_transport_controls(builder: &gtk::Builder, resource: &str) -> TransportControls {
    gtk_widgets::objects!(builder, resource, {
        transport: gtk::Box,
        transport_buttons: gtk::Fixed,
        progress_row: gtk::Box,
        elapsed: gtk::Label,
        progress: gtk::Scale,
        duration: gtk::Label,
        seek_preview: gtk::Popover,
        seek_preview_time: gtk::Label,
    });
    transport.set_width_request(BOTTOM_PLAYER_TRANSPORT_WIDTH);
    transport_buttons.set_size_request(
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        BOTTOM_PLAYER_BUTTON_ROW_HEIGHT,
    );
    progress_row.set_spacing(BOTTOM_PLAYER_PROGRESS_SPACING);
    progress.set_size_request(BOTTOM_PLAYER_PROGRESS_WIDTH, BOTTOM_PLAYER_PROGRESS_HEIGHT);
    elapsed.set_xalign(1.0);
    duration.set_xalign(0.0);
    let timestamp_size_group = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
    timestamp_size_group.add_widget(&elapsed);
    timestamp_size_group.add_widget(&duration);

    let dj_button = auto_dj_icon_button("Auto DJ");
    let previous_button = skip_icon_button(false, "Previous");
    let (play_button, play_icon) = play_icon_button("Play");
    let play_ring = crate::refill_ring::RefillRing::new(BOTTOM_PLAYER_PLAY_BUTTON_SIZE);
    let next_button = skip_icon_button(true, "Next");
    let shuffle_button = shuffle_icon_button("Shuffle");
    let repeat_button = repeat_icon_button("Repeat off");
    let random_button = random_clover_icon_button("Play random");

    configure_transport_side_button(&dj_button);
    configure_transport_side_button(&shuffle_button);
    configure_transport_side_button(&previous_button);
    configure_play_button(&play_button);
    configure_transport_side_button(&next_button);
    configure_transport_side_button(&repeat_button);
    configure_transport_side_button(&random_button);

    put_transport_button(
        &transport_buttons,
        &dj_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        -3.0,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );
    put_transport_button(
        &transport_buttons,
        &shuffle_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        -2.0,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );
    put_transport_button(
        &transport_buttons,
        &previous_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        -1.0,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );
    put_transport_button(
        &transport_buttons,
        &play_ring,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        0.0,
        BOTTOM_PLAYER_PLAY_BUTTON_SIZE,
    );
    put_transport_button(
        &transport_buttons,
        &play_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        0.0,
        BOTTOM_PLAYER_PLAY_BUTTON_SIZE,
    );
    put_transport_button(
        &transport_buttons,
        &next_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        1.0,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );
    put_transport_button(
        &transport_buttons,
        &repeat_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        2.0,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );
    put_transport_button(
        &transport_buttons,
        &random_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        3.0,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );

    install_scale_fill_updates(&progress);
    install_pointer_value_bubble(
        &progress,
        seek_preview,
        seek_preview_time,
        move |widget, x, _y| {
            let scale = widget.downcast_ref::<gtk::Scale>()?;
            let seconds = horizontal_scale_value_at_x(scale, x);
            scale.set_fill_level(seconds);
            let range = scale.range_rect();
            Some((
                format_duration(seconds.round() as u32),
                f64::from(range.y()) + f64::from(range.height()) / 2.0,
            ))
        },
    );

    TransportControls {
        root: transport,
        buttons: transport_buttons,
        random_button,
        previous_button,
        play_button,
        play_icon,
        play_ring,
        next_button,
        shuffle_button,
        repeat_button,
        dj_button,
        progress_row,
        elapsed,
        progress,
        duration,
        timestamp_size_group,
    }
}

pub fn build_player_action_controls(
    builder: &gtk::Builder,
    resource: &str,
) -> PlayerActionControls {
    gtk_widgets::objects!(builder, resource, {
        actions: gtk::Overlay,
        action_buttons: gtk::Box,
        settings_button: gtk::MenuButton,
        volume_group: gtk::Box,
        volume: gtk::Scale,
        volume_preview: gtk::Popover,
        volume_preview_level: gtk::Label,
    });
    let root = actions;
    let buttons = action_buttons;
    buttons.set_spacing(BOTTOM_PLAYER_ACTION_SPACING);
    let rating = RatingControl::new(None, true);
    rating
        .widget()
        .set_size_request(BOTTOM_PLAYER_RATING_WIDTH, BOTTOM_PLAYER_RATING_HEIGHT);
    rating.widget().set_halign(gtk::Align::End);
    rating.widget().set_valign(gtk::Align::Start);
    rating.widget().set_margin_top(5);
    rating.widget().set_margin_end(15);
    rating.widget().set_visible(false);
    root.add_overlay(rating.widget());
    root.set_measure_overlay(rating.widget(), false);

    bind_widget_tooltip(&settings_button, "Settings");
    bind_widget_accessible_label(&settings_button, "Settings");
    volume_group.set_spacing(BOTTOM_PLAYER_VOLUME_SPACING);
    volume_group.set_valign(gtk::Align::Center);
    let favorite_button = favorite_icon_button("Favorite");
    favorite_button.add_css_class("player-favorite-button");
    configure_player_action_button(&favorite_button);
    volume_group.append(&favorite_button);
    let (queue_button, queue_icon) = queue_sidebar_button("Hide sidebar");
    configure_player_action_button(&queue_button);
    volume_group.append(&queue_button);
    let (mute_button, mute_icon, mute_icon_state) = volume_icon_button("Mute");
    mute_button.add_css_class("player-mute-button");
    configure_player_action_button(&mute_button);
    volume_group.append(&mute_button);
    let volume_slot = bottom_player_allocated_slot(&volume, BOTTOM_PLAYER_VOLUME_SLOT_WIDTH, 1);
    volume_slot.add_css_class("volume-slider-slot");
    volume_slot.set_valign(gtk::Align::Fill);
    volume_group.append(&volume_slot);
    install_scale_fill_updates(&volume);
    volume_preview.set_parent(&volume);
    volume_preview.set_offset(0, -4);
    volume
        .adjustment()
        .bind_property("value", &volume_preview_level, "label")
        .transform_to(|_, value: f64| Some(format!("{:.0}", value * 100.0)))
        .sync_create()
        .build();
    let preview = volume_preview.downgrade();
    volume.connect_value_changed(move |scale| {
        if let Some(preview) = preview.upgrade() {
            point_value_bubble_at_slider(scale, &preview);
        }
    });
    let motion = gtk::EventControllerMotion::new();
    motion.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_volume = volume.downgrade();
    let weak_slot = volume_slot.downgrade();
    let preview = volume_preview.downgrade();
    let show_preview = Rc::new(move |x: f64, y: f64| {
        if let (Some(volume), Some(slot), Some(preview)) = (
            weak_volume.upgrade(),
            weak_slot.upgrade(),
            preview.upgrade(),
        ) && volume.is_sensitive()
        {
            if slot.contains(x, y)
                && let Some(point) =
                    slot.compute_point(&volume, &gtk::graphene::Point::new(x as f32, y as f32))
            {
                volume.set_fill_level(horizontal_scale_value_at_x(&volume, f64::from(point.x())));
            }
            point_value_bubble_at_slider(&volume, &preview);
            if !preview.is_visible() {
                preview.popup();
            }
        }
    });
    let enter_preview = Rc::clone(&show_preview);
    motion.connect_enter(move |_, x, y| enter_preview(x, y));
    motion.connect_motion(move |_, x, y| show_preview(x, y));
    install_value_bubble_dismissal(&motion, &volume_preview);
    volume_slot.add_controller(motion);
    volume.connect_destroy(move |_| volume_preview.unparent());
    PlayerActionControls {
        root,
        buttons,
        queue_button,
        queue_icon,
        favorite_button,
        rating,
        mute_button,
        mute_icon,
        mute_icon_state,
        volume,
        settings_button,
    }
}

pub fn configure_transport_side_button(button: &gtk::Button) {
    button.add_css_class("player-transport-button");
    button.set_size_request(
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );
}

pub fn configure_play_button(button: &gtk::Button) {
    button.add_css_class("player-transport-button");
    button.add_css_class("player-play-button");
    button.set_size_request(
        BOTTOM_PLAYER_PLAY_BUTTON_SIZE,
        BOTTOM_PLAYER_PLAY_BUTTON_SIZE,
    );
}

pub fn configure_player_action_button(button: &gtk::Button) {
    button.set_valign(gtk::Align::Center);
}

pub fn bottom_player_action_count(actions: BottomPlayerActions) -> i32 {
    match actions {
        BottomPlayerActions::Volume => 0,
        BottomPlayerActions::Favorite => 1,
        BottomPlayerActions::Queue => 2,
    }
}

pub fn bottom_player_action_min_width(actions: BottomPlayerActions) -> i32 {
    let visible_action_count = bottom_player_action_count(actions);
    BOTTOM_PLAYER_VOLUME_GROUP_MIN_WIDTH
        + BOTTOM_PLAYER_ACTION_BUTTON_SIZE * visible_action_count
        + BOTTOM_PLAYER_ACTION_SPACING * visible_action_count
}

pub fn bottom_player_side_width(player_width: i32) -> i32 {
    bottom_player_content_width(player_width).saturating_sub(BOTTOM_PLAYER_TRANSPORT_WIDTH) / 2
}

pub fn bottom_player_content_width(player_width: i32) -> i32 {
    (player_width - BOTTOM_PLAYER_EDGE_PADDING * 2).max(1)
}

pub fn bottom_player_progress_width(player_width: i32) -> i32 {
    let extra_width = player_width
        .saturating_sub(BOTTOM_PLAYER_COMPACT_MIN_WIDTH)
        .max(0);
    // Feishin gives the seekbar 95% of its one-third center column. Apply the same
    // share to width beyond Rufin's compact layout so the bar grows smoothly.
    BOTTOM_PLAYER_PROGRESS_MIN_WIDTH
        + extra_width * BOTTOM_PLAYER_PROGRESS_CENTER_PERCENT / (100 * BOTTOM_PLAYER_LAYOUT_COLUMNS)
}

pub fn centered_progress_width(
    desired_width: i32,
    content_width: i32,
    action_width: i32,
    progress_row_fixed_width: i32,
) -> i32 {
    let symmetric_capacity = content_width
        .saturating_sub(action_width.saturating_mul(2))
        .saturating_sub(progress_row_fixed_width)
        .max(1);
    desired_width.min(symmetric_capacity)
}

pub fn widget_natural_width(widget: &impl IsA<gtk::Widget>) -> i32 {
    let (_, natural, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
    natural
}

pub fn bottom_player_actions(player_width: i32) -> BottomPlayerActions {
    let side_width = bottom_player_side_width(player_width);
    if side_width >= bottom_player_action_min_width(BottomPlayerActions::Queue) {
        BottomPlayerActions::Queue
    } else if side_width >= bottom_player_action_min_width(BottomPlayerActions::Favorite) {
        BottomPlayerActions::Favorite
    } else {
        BottomPlayerActions::Volume
    }
}

pub fn bottom_player_tiny(player_width: i32) -> bool {
    player_width < BOTTOM_PLAYER_TINY_WIDTH
}

pub fn put_transport_button(
    buttons: &gtk::Fixed,
    button: &impl IsA<gtk::Widget>,
    width: i32,
    slot: f64,
    size: i32,
) {
    let (x, y) = transport_button_position(width, slot, size);
    buttons.put(button, x, y);
}

pub fn move_transport_button(
    buttons: &gtk::Fixed,
    button: &impl IsA<gtk::Widget>,
    width: i32,
    slot: f64,
    size: i32,
) {
    let (x, y) = transport_button_position(width, slot, size);
    buttons.move_(button, x, y);
}

pub fn transport_button_position(width: i32, slot: f64, size: i32) -> (f64, f64) {
    // Edges, not centers, are spaced evenly, so the larger play button keeps the same gap.
    let gap = f64::from(BOTTOM_PLAYER_BUTTON_GAP);
    let offset = if slot == 0.0 {
        0.0
    } else {
        slot.signum()
            * (f64::from(BOTTOM_PLAYER_PLAY_BUTTON_SIZE + BOTTOM_PLAYER_SIDE_BUTTON_SIZE) / 2.0
                + gap
                + (slot.abs() - 1.0) * (f64::from(BOTTOM_PLAYER_SIDE_BUTTON_SIZE) + gap))
    };
    let center_x = f64::from(width) / 2.0 + offset;
    let radius = f64::from(size) / 2.0;
    let y = f64::from(BOTTOM_PLAYER_BUTTON_ROW_HEIGHT - size) / 2.0 + BOTTOM_PLAYER_BUTTON_OFFSET_Y;
    (center_x - radius, y)
}

use crate::{progress::seekbar_target_seconds, state::NowPlayingPresentation};
use gtk_widgets::{controls::set_active_class, format_duration, route::Route};
impl crate::PlayerUi {
    pub fn update_bottom_player(self: &Rc<Self>) {
        let player = self.selected_playback().as_deref().cloned();
        let presentation = NowPlayingPresentation::new(player.as_ref());
        self.update_bottom_player_view(player.as_ref(), Some(&presentation));
    }

    pub fn update_bottom_player_with(
        self: &Rc<Self>,
        player: &PlaybackView,
        presentation: &NowPlayingPresentation,
    ) {
        self.update_bottom_player_view(Some(player), Some(presentation));
    }

    pub fn update_bottom_player_transport(self: &Rc<Self>) {
        let player = self.selected_playback().as_deref().cloned();
        self.update_bottom_player_view(player.as_ref(), None);
    }

    pub fn update_bottom_player_position(&self) {
        let (position_seconds, duration_seconds) = self
            .selected_playback()
            .as_deref()
            .map(|player| {
                (
                    (player.transport.position_millis / 1_000).min(u64::from(u32::MAX)) as u32,
                    (player.transport.duration_millis / 1_000).min(u64::from(u32::MAX)) as u32,
                )
            })
            .unwrap_or_default();
        let controls = &self.views.player_controls;
        let displayed_seconds = self.seek_preview_seconds().unwrap_or(position_seconds);
        self.controls.updating_controls.set(true);
        let time_chars = (format_duration(duration_seconds).chars().count() as i32).max(4);
        controls.elapsed.set_width_chars(time_chars);
        controls.duration.set_width_chars(time_chars);
        controls
            .elapsed
            .set_text(&format_duration(displayed_seconds));
        controls
            .progress
            .set_value(f64::from(displayed_seconds.min(duration_seconds)));
        self.controls.updating_controls.set(false);
    }

    fn update_bottom_player_view(
        self: &Rc<Self>,
        player: Option<&PlaybackView>,
        presentation: Option<&NowPlayingPresentation>,
    ) {
        let state = player
            .map(|player| player.transport.effective_state())
            .unwrap_or(TransportStatus::Stopped);
        let duration_seconds = player
            .map(|player| {
                (player.transport.duration_millis / 1_000).min(u64::from(u32::MAX)) as u32
            })
            .unwrap_or_default();
        let position_seconds = player
            .map(|player| {
                (player.transport.position_millis / 1_000).min(u64::from(u32::MAX)) as u32
            })
            .unwrap_or_default();
        let repeat_mode = player
            .map(|player| player.controls.repeat_mode)
            .unwrap_or(RepeatMode::Off);
        let controls = &self.views.player_controls;
        self.controls.updating_controls.set(true);

        if let Some(presentation) = presentation {
            self.apply_bottom_now_playing(presentation);
        }

        set_play_icon(
            &controls.play_icon,
            &controls.play_ring,
            matches!(
                state,
                TransportStatus::Resolving | TransportStatus::Playing | TransportStatus::Buffering
            ),
            player.is_some_and(|player| player.transport.refilling()),
        );
        controls
            .play_button
            .set_tooltip_text(Some(&playback_state_label(state)));

        let shuffle_enabled = player.is_some_and(|player| player.controls.shuffle_enabled);
        let auto_dj_enabled = player.is_some_and(|player| player.controls.auto_dj_enabled);
        set_active_class(&controls.shuffle_button, shuffle_enabled);
        set_active_class(&controls.repeat_button, repeat_mode != RepeatMode::Off);
        set_repeat_button_icon(&controls.repeat_button, repeat_mode);
        set_active_class(&controls.dj_button, auto_dj_enabled);
        controls.dj_button.set_tooltip_text(Some(&"Auto DJ".to_string()));
        controls
            .repeat_button
            .set_tooltip_text(Some(&repeat_label(repeat_mode)));

        let time_chars = (format_duration(duration_seconds).chars().count() as i32).max(4);
        controls.elapsed.set_width_chars(time_chars);
        controls.duration.set_width_chars(time_chars);
        let preview_seconds = self.seek_preview_seconds();
        let displayed_seconds = preview_seconds.unwrap_or(position_seconds);
        controls
            .elapsed
            .set_text(&format_duration(displayed_seconds));
        let max = f64::from(duration_seconds.max(1));
        controls.progress.set_range(0.0, max);
        let can_seek =
            player.is_some_and(|player| player.transport.can_seek && duration_seconds > 0);
        controls.progress.set_sensitive(can_seek);
        controls
            .progress
            .set_value(f64::from(displayed_seconds.min(duration_seconds)));
        controls
            .duration
            .set_text(&format_duration(duration_seconds));

        let (muted, volume) = player
            .map(|player| (player.controls.muted, player.controls.volume))
            .unwrap_or((false, 1.0));
        let volume_icon = volume_icon_state(muted, volume);
        if controls.mute_icon_state.get() != volume_icon {
            controls.mute_icon_state.set(volume_icon);
            set_volume_icon(&controls.mute_icon, volume_icon);
        }
        controls.volume.set_value(volume);
        self.controls.updating_controls.set(false);
    }

    fn apply_bottom_now_playing(self: &Rc<Self>, presentation: &NowPlayingPresentation) {
        let controls = &self.views.player_controls;
        let window_title = crate::state::playback_window_title(
            presentation.current.then_some(presentation.title.as_str()),
            presentation.current.then_some(presentation.artist.as_str()),
        );
        if let Some(window) = self.window.upgrade() {
            window.set_title(Some(&window_title));
        }
        if presentation.current {
            self.artwork.bind_playback_artwork_tile(
                &controls.cover,
                presentation.artwork.clone(),
                BOTTOM_PLAYER_COVER_SIZE,
            );
        } else {
            self.artwork.clear_artwork_tile(&controls.cover);
        }
        controls.title.set_text(&presentation.title);
        if let Some(artist_links) = controls.artist_links.borrow().as_ref() {
            artist_links.bind(presentation.artist_links.clone());
        } else {
            controls.artist.set_text(&presentation.artist);
        }
        if let Some(album_links) = controls.album_links.borrow().as_ref() {
            album_links.bind(presentation.album_links.clone());
        } else {
            controls.album.set_text(&presentation.album);
        }
        controls.title.set_sensitive(presentation.current);
        controls.menu_button.set_sensitive(presentation.current);
        controls
            .artist
            .set_sensitive(presentation.current && !presentation.artist.is_empty());
        controls
            .album
            .set_sensitive(presentation.current && !presentation.album.is_empty());
        let rating_visible =
            self.settings.current.borrow().show_bottom_bar_rating && presentation.current;
        controls.rating.widget().set_visible(rating_visible);
        controls.rating.widget().set_sensitive(rating_visible);
        controls.action_buttons.set_margin_top(if rating_visible {
            BOTTOM_PLAYER_ACTION_ROW_OFFSET_Y * 2
        } else {
            0
        });
        (self.refresh_current_controls)();
    }

    pub fn set_bottom_player_favorite(&self, target: &library::FavoriteTarget, value: bool) {
        let library::FavoriteTarget::Track(media_uri) = target else {
            return;
        };
        let current_matches = self
            .selected_playback()
            .as_deref()
            .and_then(|player| player.transport.current.as_ref())
            .is_some_and(|current| current.row.track.media_uri == *media_uri);
        if current_matches {
            set_favorite_button_active(&self.views.player_controls.favorite_button, value);
        }
    }

    pub fn navigate_current_track_album(self: &Rc<Self>) {
        let Some(current) = self
            .selected_playback()
            .as_deref()
            .and_then(|player| player.transport.current.as_ref())
            .cloned()
        else {
            return;
        };
        if let Some(album) = &current.row.track.links.album_media_uri {
            (self.navigate)(Route::AlbumDetail(album.clone()));
        }
    }

    pub fn maybe_clear_player_seek_preview(&self, player: &PlaybackView, track_changed: bool) {
        let Some(target_seconds) = self.seek_preview_seconds() else {
            return;
        };
        if should_clear_seek_preview(
            target_seconds,
            player.transport.position_millis,
            track_changed,
            player.transport.current.is_some(),
            player.transport.can_seek,
            self.controls.seek_pointer_active.get(),
        ) {
            self.clear_player_seek_preview();
        }
    }

    fn clear_player_seek_preview(&self) {
        self.controls.seek_pointer_active.set(false);
        self.set_seek_preview_seconds(None);
    }
}

fn should_clear_seek_preview(
    target_seconds: u32,
    position_millis: u64,
    track_changed: bool,
    has_current: bool,
    can_seek: bool,
    pointer_active: bool,
) -> bool {
    track_changed
        || !has_current
        || !can_seek
        || (!pointer_active
            && rufin_core::playback::seek_preview_matches_position(
                u64::from(target_seconds) * 1_000,
                position_millis,
            ))
}

fn playback_state_label(state: TransportStatus) -> String {
    match state {
        TransportStatus::Stopped | TransportStatus::Failed => "Play".to_string(),
        TransportStatus::Paused => "Resume".to_string(),
        TransportStatus::Resolving | TransportStatus::Buffering | TransportStatus::Playing => {
            "Pause".to_string()
        }
    }
}

fn repeat_label(repeat_mode: RepeatMode) -> String {
    match repeat_mode {
        RepeatMode::Off => "Repeat off".to_string(),
        RepeatMode::One => "Repeat one".to_string(),
        RepeatMode::All => "Repeat all".to_string(),
    }
}

pub fn preview_player_seek(shell: &Rc<crate::PlayerUi>, seconds: u32) {
    shell.set_seek_preview_seconds(Some(seconds));
    shell
        .views
        .player_controls
        .progress
        .set_value(f64::from(seconds));
    shell
        .views
        .player_controls
        .elapsed
        .set_text(&format_duration(seconds));
}

pub fn commit_player_seek_preview_now(shell: &Rc<crate::PlayerUi>) {
    let Some(seconds) = shell.seek_preview_seconds() else {
        return;
    };
    shell.playback_owner.seek_seconds(seconds);
}

impl crate::PlayerUi {
    pub fn apply_user_volume(self: &Rc<Self>, volume: f64) {
        let volume = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.playback_owner.set_volume(volume);
        self.settings.current.borrow_mut().playback.volume = volume;
        if let Some(source) = self.controls.volume_persist_source.borrow_mut().take() {
            source.remove();
        }
        let shell = Rc::clone(self);
        let source = glib::timeout_add_local_once(VOLUME_PERSIST_DELAY, move || {
            *shell.controls.volume_persist_source.borrow_mut() = None;
            shell.playback_owner.persist_volume(volume);
        });
        *self.controls.volume_persist_source.borrow_mut() = Some(source);
    }

    pub fn apply_user_muted(&self, muted: bool) {
        self.playback_owner.set_muted(muted);
        self.settings.current.borrow_mut().playback.muted = muted;
    }
}

pub fn connect_bottom_player_resize(shell: &Rc<crate::PlayerUi>) {
    if let Some(window) = shell.window.upgrade() {
        let weak = Rc::downgrade(shell);
        let watch_scale = move |window: &gtk::ApplicationWindow| {
            if let Some(surface) = window.surface() {
                let weak = weak.clone();
                surface.connect_scale_notify(move |_| {
                    if let Some(shell) = weak.upgrade() {
                        shell.update_bottom_player();
                    }
                });
            }
        };
        if window.is_realized() {
            watch_scale(&window);
        }
        window.connect_realize(watch_scale);
    }
    let resize_shell = Rc::downgrade(shell);
    let allocated_width = Cell::new(0);
    shell
        .views
        .player_controls
        .root
        .set_size_callback(move |width, _height| {
            if width <= 0 || allocated_width.replace(width) == width {
                return;
            }
            let Some(resize_shell) = resize_shell.upgrade() else {
                return;
            };
            resize_shell.apply_bottom_player_width(width);
        });
}

impl crate::PlayerUi {
    pub fn apply_bottom_player_width(&self, player_width: i32) {
        if player_width > 0 {
            let tiny = bottom_player_tiny(player_width);
            let actions = bottom_player_actions(player_width);
            self.apply_bottom_player_tiny(tiny);
            self.apply_bottom_player_actions(actions);
            let player = &self.views.player_controls;
            let desired_progress_width = bottom_player_progress_width(player_width);
            let progress_width = if tiny {
                desired_progress_width
            } else {
                // Keep the action surface inside its equal side allocation. The progress row yields
                // when necessary so the two side slots remain the same width around the transport.
                let action_width = widget_natural_width(&player.actions);
                let progress_row_fixed_width = widget_natural_width(&player.elapsed)
                    + widget_natural_width(&player.duration)
                    + BOTTOM_PLAYER_PROGRESS_SPACING * 2;
                centered_progress_width(
                    desired_progress_width,
                    bottom_player_content_width(player_width),
                    action_width,
                    progress_row_fixed_width,
                )
            };
            self.views
                .player_controls
                .progress
                .set_size_request(progress_width, BOTTOM_PLAYER_PROGRESS_HEIGHT);
        }
    }

    fn apply_bottom_player_tiny(&self, tiny: bool) {
        let player = &self.views.player_controls;
        let transport_width = if tiny {
            BOTTOM_PLAYER_TINY_TRANSPORT_WIDTH
        } else {
            BOTTOM_PLAYER_TRANSPORT_WIDTH
        };
        player.transport.set_width_request(transport_width);
        player.transport_slot.set_width_request(transport_width);
        player
            .transport_buttons
            .set_size_request(transport_width, BOTTOM_PLAYER_BUTTON_ROW_HEIGHT);
        player.now_playing_wall.set_width_request(1);
        player.transport_slot.set_halign(if tiny {
            gtk::Align::End
        } else {
            gtk::Align::Center
        });
        move_transport_button(
            &player.transport_buttons,
            &player.previous_button,
            transport_width,
            -1.0,
            BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
        );
        move_transport_button(
            &player.transport_buttons,
            &player.play_ring,
            transport_width,
            0.0,
            BOTTOM_PLAYER_PLAY_BUTTON_SIZE,
        );
        move_transport_button(
            &player.transport_buttons,
            &player.play_button,
            transport_width,
            0.0,
            BOTTOM_PLAYER_PLAY_BUTTON_SIZE,
        );
        move_transport_button(
            &player.transport_buttons,
            &player.next_button,
            transport_width,
            1.0,
            BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
        );
        player.dj_button.set_visible(!tiny);
        player.shuffle_button.set_visible(!tiny);
        player.repeat_button.set_visible(!tiny);
        player.random_button.set_visible(!tiny);
        player.progress_row.set_visible(!tiny);
        player.actions.set_visible(!tiny);
        player.right_slot.set_visible(!tiny);
    }

    fn apply_bottom_player_actions(&self, actions: BottomPlayerActions) {
        let player = &self.views.player_controls;
        player.favorite_button.set_visible(matches!(
            actions,
            BottomPlayerActions::Favorite | BottomPlayerActions::Queue
        ));
        player
            .queue_button
            .set_visible(matches!(actions, BottomPlayerActions::Queue));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn bottom_player_actions_never_outgrow_their_equal_side() {
        for player_width in super::BOTTOM_PLAYER_TINY_WIDTH..=4096 {
            let actions = super::bottom_player_actions(player_width);
            let action_width = super::bottom_player_action_min_width(actions);

            assert!(
                action_width <= super::bottom_player_side_width(player_width),
                "actions outgrew their side at player width {player_width}"
            );
        }
    }

    #[test]
    fn non_tiny_player_preserves_now_playing_identity_width() {
        let fixed_now_playing_width = super::BOTTOM_PLAYER_HORIZONTAL_PADDING
            + super::BOTTOM_PLAYER_COVER_SIZE
            + super::BOTTOM_PLAYER_NOW_PLAYING_SPACING;
        let identity_width = super::bottom_player_side_width(super::BOTTOM_PLAYER_TINY_WIDTH)
            - fixed_now_playing_width;

        assert!(identity_width >= super::BOTTOM_PLAYER_IDENTITY_MIN_WIDTH);
        assert!(super::bottom_player_tiny(
            super::BOTTOM_PLAYER_TINY_WIDTH - 1
        ));
        assert!(!super::bottom_player_tiny(super::BOTTOM_PLAYER_TINY_WIDTH));
    }

    #[test]
    fn bottom_player_actions_use_the_available_side_width() {
        let tiers = [
            (
                super::BottomPlayerActions::Favorite,
                super::BottomPlayerActions::Volume,
            ),
            (
                super::BottomPlayerActions::Queue,
                super::BottomPlayerActions::Favorite,
            ),
        ];

        for (tier, previous_tier) in tiers {
            let threshold = super::BOTTOM_PLAYER_EDGE_PADDING * 2
                + super::BOTTOM_PLAYER_TRANSPORT_WIDTH
                + super::bottom_player_action_min_width(tier) * 2;

            assert_eq!(super::bottom_player_actions(threshold - 1), previous_tier);
            assert_eq!(super::bottom_player_actions(threshold), tier);
        }
    }

    #[test]
    fn bottom_player_progress_width_scales_with_the_center_column() {
        assert_eq!(
            super::bottom_player_progress_width(super::BOTTOM_PLAYER_COMPACT_MIN_WIDTH - 50),
            super::BOTTOM_PLAYER_PROGRESS_MIN_WIDTH
        );
        assert_eq!(
            super::bottom_player_progress_width(super::BOTTOM_PLAYER_COMPACT_MIN_WIDTH),
            super::BOTTOM_PLAYER_PROGRESS_MIN_WIDTH
        );
        assert_eq!(
            super::bottom_player_progress_width(super::BOTTOM_PLAYER_COMPACT_MIN_WIDTH + 300),
            super::BOTTOM_PLAYER_PROGRESS_MIN_WIDTH + 95
        );
        assert!(
            super::bottom_player_progress_width(1024)
                > super::bottom_player_progress_width(super::BOTTOM_PLAYER_COMPACT_MIN_WIDTH)
        );
        assert!(
            super::bottom_player_progress_width(1920) > super::bottom_player_progress_width(1024)
        );
    }

    #[test]
    fn centered_progress_budget_preserves_both_side_allocations() {
        for desired_width in (1..=512).step_by(17) {
            for action_width in (0..=320).step_by(11) {
                for fixed_width in (0..=128).step_by(7) {
                    for available_progress in [1, desired_width / 2 + 1, desired_width, 640] {
                        let content_width = action_width * 2 + fixed_width + available_progress;
                        let progress_width = super::centered_progress_width(
                            desired_width,
                            content_width,
                            action_width,
                            fixed_width,
                        );

                        assert!(progress_width <= desired_width);
                        assert!(
                            progress_width + fixed_width + action_width * 2 <= content_width,
                            "center claimed side allocation: content={content_width}, actions={action_width}, fixed={fixed_width}, desired={desired_width}, progress={progress_width}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn player_volume_uses_the_matching_symbolic_icon() {
        assert_eq!(
            super::volume_icon_state(false, 0.0),
            super::VolumeIcon::Muted
        );
        assert_eq!(
            super::volume_icon_state(false, 0.01),
            super::VolumeIcon::Low
        );
        assert_eq!(
            super::volume_icon_state(false, 0.5),
            super::VolumeIcon::Medium
        );
        assert_eq!(
            super::volume_icon_state(false, 0.9),
            super::VolumeIcon::High
        );
        assert_eq!(
            super::volume_icon_state(true, 1.0),
            super::VolumeIcon::Muted
        );
    }

    #[test]
    fn seek_preview_waits_for_backend_acknowledgement() {
        assert!(!super::should_clear_seek_preview(
            19, 5_000, false, true, true, false
        ));
        assert!(super::should_clear_seek_preview(
            19, 19_000, false, true, true, false
        ));
        assert!(super::should_clear_seek_preview(
            19, 5_000, true, true, true, false
        ));
        assert!(super::should_clear_seek_preview(
            19, 5_000, false, false, true, false
        ));
        assert!(super::should_clear_seek_preview(
            19, 5_000, false, true, false, false
        ));
    }

    #[test]
    fn seek_preview_stays_under_the_pointer_until_release() {
        assert!(!super::should_clear_seek_preview(
            19, 19_000, false, true, true, true
        ));
        assert!(super::should_clear_seek_preview(
            19, 19_000, false, true, true, false
        ));
        assert!(super::should_clear_seek_preview(
            19, 5_000, true, true, true, true
        ));
    }
}

pub fn connect_player_controls(shell: &Rc<crate::PlayerUi>) {
    connect_bottom_player_resize(shell);
    let fullscreen = &shell.views.fullscreen_player;
    for (label, binding) in [
        (&fullscreen.artist, &fullscreen.artist_links),
        (&fullscreen.album, &fullscreen.album_links),
    ] {
        let weak = Rc::downgrade(shell);
        binding.replace(Some(DetailLinkBinding::new(
            label,
            Rc::new(move |route| {
                if let Some(shell) = weak.upgrade() {
                    shell.close_fullscreen_player();
                    (shell.navigate)(route);
                }
            }),
        )));
    }
    let controls = &shell.views.player_controls;
    controls.artist_links.replace(Some(DetailLinkBinding::new(
        &controls.artist,
        Rc::clone(&shell.navigate),
    )));
    controls.album_links.replace(Some(DetailLinkBinding::new(
        &controls.album,
        Rc::clone(&shell.navigate),
    )));
    let transport = shell.playback_owner.clone();
    shell
        .views
        .player_controls
        .previous_button
        .connect_clicked(move |_| transport.previous());

    let transport = shell.playback_owner.clone();
    shell
        .views
        .player_controls
        .play_button
        .connect_clicked(move |_| transport.play_pause());

    let transport = shell.playback_owner.clone();
    shell
        .views
        .player_controls
        .next_button
        .connect_clicked(move |_| transport.next());

    let feedback_shell = Rc::clone(shell);
    shell
        .views
        .player_controls
        .shuffle_button
        .connect_clicked(move |_| {
            let Some(enabled) = feedback_shell
                .selected_playback()
                .as_deref()
                .map(|player| !player.controls.shuffle_enabled)
            else {
                return;
            };
            feedback_shell.playback_owner.set_shuffle(enabled);
        });

    let feedback_shell = Rc::clone(shell);
    shell
        .views
        .player_controls
        .repeat_button
        .connect_clicked(move |_| {
            if feedback_shell.selected_playback().is_none() {
                return;
            }
            feedback_shell.playback_owner.cycle_repeat();
        });

    let feedback_shell = Rc::clone(shell);
    shell
        .views
        .player_controls
        .dj_button
        .connect_clicked(move |_| {
            if feedback_shell.selected_playback().is_none() {
                return;
            }
            feedback_shell.playback_owner.toggle_auto_dj();
        });

    let mute_shell = Rc::clone(shell);
    shell
        .views
        .player_controls
        .mute_button
        .connect_clicked(move |_| {
            let Some(muted) = mute_shell
                .selected_playback()
                .as_deref()
                .map(|player| !player.controls.muted)
            else {
                return;
            };
            mute_shell.apply_user_muted(muted);
        });
    let seek_shell = Rc::downgrade(shell);
    shell
        .views
        .player_controls
        .progress
        .connect_change_value(move |scale, _, value| {
            let Some(seek_shell) = seek_shell.upgrade() else {
                return glib::Propagation::Stop;
            };
            if seek_shell.controls.updating_controls.get() {
                return glib::Propagation::Proceed;
            }
            let duration_seconds = {
                let player = seek_shell.selected_playback();
                let Some(player) = player.as_ref() else {
                    return glib::Propagation::Stop;
                };
                let duration_seconds =
                    (player.transport.duration_millis / 1_000).min(u64::from(u32::MAX)) as u32;
                if player.transport.current.is_none()
                    || !player.transport.can_seek
                    || duration_seconds == 0
                {
                    return glib::Propagation::Stop;
                }
                duration_seconds
            };

            let seconds = seekbar_target_seconds(value, duration_seconds);
            preview_player_seek(&seek_shell, seconds);
            scale.set_value(f64::from(seconds));
            if !seek_shell.controls.seek_pointer_active.get() {
                commit_player_seek_preview_now(&seek_shell);
            }
            glib::Propagation::Stop
        });

    for widget in [shell
        .views
        .player_controls
        .progress
        .upcast_ref::<gtk::Widget>()]
    {
        // Observe release even after the scale's own gesture claims the drag.
        let events = gtk::EventControllerLegacy::new();
        events.set_propagation_phase(gtk::PropagationPhase::Capture);
        let seek_shell = Rc::downgrade(shell);
        events.connect_event(move |_, event| {
            let Some(shell) = seek_shell.upgrade() else {
                return glib::Propagation::Proceed;
            };
            match event.event_type() {
                gtk::gdk::EventType::ButtonPress | gtk::gdk::EventType::TouchBegin => {
                    shell.controls.seek_pointer_active.set(true);
                }
                gtk::gdk::EventType::ButtonRelease | gtk::gdk::EventType::TouchEnd => {
                    if shell.controls.seek_pointer_active.replace(false) {
                        commit_player_seek_preview_now(&shell);
                    }
                }
                gtk::gdk::EventType::TouchCancel | gtk::gdk::EventType::GrabBroken => {
                    shell.clear_player_seek_preview();
                }
                _ => {}
            }
            glib::Propagation::Proceed
        });
        widget.add_controller(events);
    }

    let volume_shell = Rc::clone(shell);
    shell
        .views
        .player_controls
        .volume
        .connect_value_changed(move |scale| {
            if volume_shell.controls.updating_controls.get() {
                return;
            }
            volume_shell.apply_user_volume(scale.value());
        });
}

impl crate::PlayerUi {
    pub fn set_current_rating_and_favorite(
        &self,
        favorite: bool,
        rating: Option<u8>,
        half_stars: bool,
    ) {
        let controls = &self.views.player_controls;
        set_favorite_button_active(&controls.favorite_button, favorite);
        controls.rating.set_rating(rating, half_stars);
    }
}
