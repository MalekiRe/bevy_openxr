use bevy::{
    prelude::*,
    render::{
        camera::{ManualTextureView, ManualTextureViewHandle, ManualTextureViews, RenderTarget},
        extract_resource::ExtractResourcePlugin,
        renderer::render_system,
        view::ExtractedView,
        Render, RenderApp, RenderSet,
    },
    transform::TransformSystem,
};
use bevy_xr_api::camera::{XrCamera, XrCameraBundle, XrProjection};
use openxr::{CompositionLayerFlags, ViewStateFlags};

use crate::oxr::init::{session_started, OXrPreUpdateSet};
use crate::oxr::layer_builder::*;
use crate::oxr::resources::*;

pub struct XrRenderPlugin;

impl Plugin for XrRenderPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((ExtractResourcePlugin::<OXrViews>::default(),))
            .add_systems(
                PreUpdate,
                (
                    init_views.run_if(resource_added::<OXrGraphicsInfo>),
                    wait_frame.run_if(session_started),
                    locate_views.run_if(session_started),
                    update_views.run_if(session_started),
                )
                    .chain()
                    .after(OXrPreUpdateSet::HandleEvents),
            )
            .add_systems(
                PostUpdate,
                (locate_views, update_views)
                    .chain()
                    .run_if(session_started)
                    .before(TransformSystem::TransformPropagate),
            );
        app.sub_app_mut(RenderApp).add_systems(
            Render,
            (
                (
                    insert_texture_views,
                    locate_views,
                    update_views_render_world,
                )
                    .chain()
                    .in_set(RenderSet::PrepareAssets),
                begin_frame
                    .before(RenderSet::Queue)
                    .before(insert_texture_views),
                wait_image.in_set(RenderSet::Render).before(render_system),
                (end_frame).chain().in_set(RenderSet::Cleanup),
            )
                .run_if(session_started),
        );
    }
}

pub const XR_TEXTURE_INDEX: u32 = 3383858418;

// TODO: have cameras initialized externally and then recieved by this function.
/// This is needed to properly initialize the texture views so that bevy will set them to the correct resolution despite them being updated in the render world.
pub fn init_views(
    graphics_info: Res<OXrGraphicsInfo>,
    mut manual_texture_views: ResMut<ManualTextureViews>,
    swapchain_images: Res<OXrSwapchainImages>,
    mut commands: Commands,
) {
    let _span = info_span!("xr_init_views");
    let temp_tex = swapchain_images.first().unwrap();
    // this for loop is to easily add support for quad or mono views in the future.
    let mut views = vec![];
    for index in 0..2 {
        info!("{}", graphics_info.resolution);
        let view_handle =
            add_texture_view(&mut manual_texture_views, temp_tex, &graphics_info, index);

        commands.spawn((
            XrCameraBundle {
                camera: Camera {
                    target: RenderTarget::TextureView(view_handle),
                    ..Default::default()
                },
                view: XrCamera(index),
                ..Default::default()
            },
            // OpenXrTracker,
            // XrRoot::default(),
        ));
        views.push(default());
    }
    commands.insert_resource(OXrViews(views));
}

pub fn wait_frame(mut frame_waiter: ResMut<OXrFrameWaiter>, mut commands: Commands) {
    let _span = info_span!("xr_wait_frame");
    let state = frame_waiter.wait().expect("Failed to wait frame");
    // Here we insert the predicted display time for when this frame will be displayed.
    // TODO: don't add predicted_display_period if pipelined rendering plugin not enabled
    commands.insert_resource(OXrTime(state.predicted_display_time));
}

pub fn locate_views(
    session: Res<OXrSession>,
    stage: Res<OXrStage>,
    time: Res<OXrTime>,
    mut openxr_views: ResMut<OXrViews>,
) {
    let _span = info_span!("xr_locate_views");
    let (flags, xr_views) = session
        .locate_views(
            openxr::ViewConfigurationType::PRIMARY_STEREO,
            **time,
            &stage,
        )
        .expect("Failed to locate views");
    if openxr_views.len() != xr_views.len() {
        openxr_views.resize(xr_views.len(), default());
    }
    match (
        flags & ViewStateFlags::ORIENTATION_VALID == ViewStateFlags::ORIENTATION_VALID,
        flags & ViewStateFlags::POSITION_VALID == ViewStateFlags::POSITION_VALID,
    ) {
        (true, true) => *openxr_views = OXrViews(xr_views),
        (true, false) => {
            for (i, view) in openxr_views.iter_mut().enumerate() {
                view.pose.orientation = xr_views[i].pose.orientation;
            }
        }
        (false, true) => {
            for (i, view) in openxr_views.iter_mut().enumerate() {
                view.pose.position = xr_views[i].pose.position;
            }
        }
        (false, false) => {}
    }
}

pub fn update_views(
    mut query: Query<(&mut Transform, &mut XrProjection, &XrCamera)>,
    views: ResMut<OXrViews>,
) {
    for (mut transform, mut projection, camera) in query.iter_mut() {
        let Some(view) = views.get(camera.0 as usize) else {
            continue;
        };

        let projection_matrix = calculate_projection(projection.near, view.fov);
        projection.projection_matrix = projection_matrix;

        let openxr::Quaternionf { x, y, z, w } = view.pose.orientation;
        let rotation = Quat::from_xyzw(x, y, z, w);
        transform.rotation = rotation;
        let openxr::Vector3f { x, y, z } = view.pose.position;
        let translation = Vec3::new(x, y, z);
        transform.translation = translation;
    }
}

pub fn update_views_render_world(
    views: Res<OXrViews>,
    root: Res<OXrRootTransform>,
    mut query: Query<(&mut ExtractedView, &XrCamera)>,
) {
    for (mut extracted_view, camera) in query.iter_mut() {
        let Some(view) = views.get(camera.0 as usize) else {
            continue;
        };
        let mut transform = Transform::IDENTITY;
        let openxr::Quaternionf { x, y, z, w } = view.pose.orientation;
        let rotation = Quat::from_xyzw(x, y, z, w);
        transform.rotation = rotation;
        let openxr::Vector3f { x, y, z } = view.pose.position;
        let translation = Vec3::new(x, y, z);
        transform.translation = translation;
        extracted_view.transform = root.0.mul_transform(transform);
    }
}

fn calculate_projection(near_z: f32, fov: openxr::Fovf) -> Mat4 {
    //  symmetric perspective for debugging
    // let x_fov = (self.fov.angle_left.abs() + self.fov.angle_right.abs());
    // let y_fov = (self.fov.angle_up.abs() + self.fov.angle_down.abs());
    // return Mat4::perspective_infinite_reverse_rh(y_fov, x_fov / y_fov, self.near);

    let is_vulkan_api = false; // FIXME wgpu probably abstracts this
    let far_z = -1.; //   use infinite proj
                     // let far_z = self.far;

    let tan_angle_left = fov.angle_left.tan();
    let tan_angle_right = fov.angle_right.tan();

    let tan_angle_down = fov.angle_down.tan();
    let tan_angle_up = fov.angle_up.tan();

    let tan_angle_width = tan_angle_right - tan_angle_left;

    // Set to tanAngleDown - tanAngleUp for a clip space with positive Y
    // down (Vulkan). Set to tanAngleUp - tanAngleDown for a clip space with
    // positive Y up (OpenGL / D3D / Metal).
    // const float tanAngleHeight =
    //     graphicsApi == GRAPHICS_VULKAN ? (tanAngleDown - tanAngleUp) : (tanAngleUp - tanAngleDown);
    let tan_angle_height = if is_vulkan_api {
        tan_angle_down - tan_angle_up
    } else {
        tan_angle_up - tan_angle_down
    };

    // Set to nearZ for a [-1,1] Z clip space (OpenGL / OpenGL ES).
    // Set to zero for a [0,1] Z clip space (Vulkan / D3D / Metal).
    // const float offsetZ =
    //     (graphicsApi == GRAPHICS_OPENGL || graphicsApi == GRAPHICS_OPENGL_ES) ? nearZ : 0;
    // FIXME handle enum of graphics apis
    let offset_z = 0.;

    let mut cols: [f32; 16] = [0.0; 16];

    if far_z <= near_z {
        // place the far plane at infinity
        cols[0] = 2. / tan_angle_width;
        cols[4] = 0.;
        cols[8] = (tan_angle_right + tan_angle_left) / tan_angle_width;
        cols[12] = 0.;

        cols[1] = 0.;
        cols[5] = 2. / tan_angle_height;
        cols[9] = (tan_angle_up + tan_angle_down) / tan_angle_height;
        cols[13] = 0.;

        cols[2] = 0.;
        cols[6] = 0.;
        cols[10] = -1.;
        cols[14] = -(near_z + offset_z);

        cols[3] = 0.;
        cols[7] = 0.;
        cols[11] = -1.;
        cols[15] = 0.;

        //  bevy uses the _reverse_ infinite projection
        //  https://dev.theomader.com/depth-precision/
        let z_reversal = Mat4::from_cols_array_2d(&[
            [1f32, 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., -1., 0.],
            [0., 0., 1., 1.],
        ]);

        return z_reversal * Mat4::from_cols_array(&cols);
    } else {
        // normal projection
        cols[0] = 2. / tan_angle_width;
        cols[4] = 0.;
        cols[8] = (tan_angle_right + tan_angle_left) / tan_angle_width;
        cols[12] = 0.;

        cols[1] = 0.;
        cols[5] = 2. / tan_angle_height;
        cols[9] = (tan_angle_up + tan_angle_down) / tan_angle_height;
        cols[13] = 0.;

        cols[2] = 0.;
        cols[6] = 0.;
        cols[10] = -(far_z + offset_z) / (far_z - near_z);
        cols[14] = -(far_z * (near_z + offset_z)) / (far_z - near_z);

        cols[3] = 0.;
        cols[7] = 0.;
        cols[11] = -1.;
        cols[15] = 0.;
    }

    Mat4::from_cols_array(&cols)
}

/// # Safety
/// Images inserted into texture views here should not be written to until [`wait_image`] is ran
pub fn insert_texture_views(
    swapchain_images: Res<OXrSwapchainImages>,
    mut swapchain: ResMut<OXrSwapchain>,
    mut manual_texture_views: ResMut<ManualTextureViews>,
    graphics_info: Res<OXrGraphicsInfo>,
) {
    let _span = info_span!("xr_insert_texture_views");
    let index = swapchain.acquire_image().expect("Failed to acquire image");
    let image = &swapchain_images[index as usize];

    for i in 0..2 {
        add_texture_view(&mut manual_texture_views, image, &graphics_info, i);
    }
}

pub fn wait_image(mut swapchain: ResMut<OXrSwapchain>) {
    swapchain
        .wait_image(openxr::Duration::INFINITE)
        .expect("Failed to wait image");
}

pub fn add_texture_view(
    manual_texture_views: &mut ManualTextureViews,
    texture: &wgpu::Texture,
    info: &OXrGraphicsInfo,
    index: u32,
) -> ManualTextureViewHandle {
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2),
        array_layer_count: Some(1),
        base_array_layer: index,
        ..default()
    });
    let view = ManualTextureView {
        texture_view: view.into(),
        size: info.resolution,
        format: info.format,
    };
    let handle = ManualTextureViewHandle(XR_TEXTURE_INDEX + index);
    manual_texture_views.insert(handle, view);
    handle
}

pub fn begin_frame(mut frame_stream: ResMut<OXrFrameStream>) {
    frame_stream.begin().expect("Failed to begin frame")
}

pub fn end_frame(
    mut frame_stream: ResMut<OXrFrameStream>,
    mut swapchain: ResMut<OXrSwapchain>,
    stage: Res<OXrStage>,
    display_time: Res<OXrTime>,
    graphics_info: Res<OXrGraphicsInfo>,
    openxr_views: Res<OXrViews>,
) {
    let _span = info_span!("xr_end_frame");
    swapchain.release_image().unwrap();
    let rect = openxr::Rect2Di {
        offset: openxr::Offset2Di { x: 0, y: 0 },
        extent: openxr::Extent2Di {
            width: graphics_info.resolution.x as _,
            height: graphics_info.resolution.y as _,
        },
    };
    frame_stream
        .end(
            **display_time,
            graphics_info.blend_mode,
            &[&CompositionLayerProjection::new()
                .layer_flags(CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA)
                .space(&stage)
                .views(&[
                    CompositionLayerProjectionView::new()
                        .pose(openxr_views.0[0].pose)
                        .fov(openxr_views.0[0].fov)
                        .sub_image(
                            SwapchainSubImage::new()
                                .swapchain(&swapchain)
                                .image_array_index(0)
                                .image_rect(rect),
                        ),
                    CompositionLayerProjectionView::new()
                        .pose(openxr_views.0[1].pose)
                        .fov(openxr_views.0[1].fov)
                        .sub_image(
                            SwapchainSubImage::new()
                                .swapchain(&swapchain)
                                .image_array_index(1)
                                .image_rect(rect),
                        ),
                ])],
        )
        .expect("Failed to end frame");
}