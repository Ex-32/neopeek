//! Camera (PLAN.md §4.3): an orbital camera — target (waterfall mid-point
//! by default), azimuth θ, elevation φ (clamped ±85°), distance d
//! (clamped), fov 45°, near 0.1, far 300.
//!
//! `eye = target + d·(cosφ·sinθ, sinφ, cosφ·cosθ)`; view = look-at; proj =
//! perspective. The default state matches the M4 fixed vantage (eye 3.5
//! from the waterfall mid-line, elevation 0.55 rad) so the first paint
//! does not jump.
//!
//! All logic is headless and unit-tested; matrices are column-major for
//! direct WGSL uniform upload.

#[derive(Debug, Clone, Copy)]
pub struct Camera {
    /// Look-at point.
    pub target: [f32; 3],
    pub distance: f32,
    pub azimuth: f32,
    pub elevation: f32,
    pub fov_y: f32,
    pub near: f32,
    pub far: f32,
    pub aspect: f32,
}

/// Elevation clamp ±85° — also protects the y-up look-at from degeneracy.
pub const ELEV_MAX: f32 = 85.0f32.to_radians();
pub const DIST_MIN: f32 = 0.4;
pub const DIST_MAX: f32 = 30.0;
/// Default (sndpeek-parity) distance from the target.
pub const DISTANCE: f32 = 3.5;
/// Default elevation («slight tilt»).
pub const ELEVATION: f32 = 0.55;
/// Default fov 45°.
pub const FOV_Y: f32 = std::f32::consts::FRAC_PI_4;
pub const NEAR: f32 = 0.1;
pub const FAR: f32 = 300.0;
/// Keyboard elevation step (`[`/`]`, sndpeek `g_inc_val_kb`).
pub const TILT_STEP: f32 = 0.025;
/// Autospin yaw rate (rad/s) — slow drift like the original.
pub const AUTOSPIN_RATE: f32 = 0.5;

/// The three `X`/`Y`/`Z` snapped views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapView {
    /// `X`: from above.
    Top,
    /// `Y`: from the right (+x).
    Side,
    /// `Z`: from the front (+z).
    Front,
}

impl Camera {
    /// M5 default: eye above the origin looking slightly down the
    /// waterfall mid-line (which extends toward −z).
    pub fn default_vantage(width: u32, height: u32) -> Self {
        Self {
            target: [0.0, 0.25, -1.3],
            distance: DISTANCE,
            azimuth: 0.0,
            elevation: ELEVATION,
            fov_y: FOV_Y,
            near: NEAR,
            far: FAR,
            aspect: width as f32 / height.max(1) as f32,
        }
    }

    /// Eye position on the sphere around the target.
    pub fn eye(&self) -> [f32; 3] {
        let [tx, ty, tz] = self.target;
        let (cc, cs) = (self.elevation.cos(), self.elevation.sin());
        let d = self.distance;
        [
            tx + d * cc * self.azimuth.sin(),
            ty + d * cs,
            tz + d * cc * self.azimuth.cos(),
        ]
    }

    /// Full view-projection matrix (column-major), ready for a shader.
    pub fn view_proj(&self) -> [f32; 16] {
        mul(proj_matrix(self), view_matrix(self.eye(), self.target))
    }

    // ---- interaction (M5) -------------------------------------------

    /// Rotate on the sphere. Azimuth wraps positively unbounded; elevation
    /// clamps to ±85°.
    pub fn orbit(&mut self, dtheta: f32, dphi: f32) {
        self.azimuth += dtheta;
        self.elevation = (self.elevation + dphi).clamp(-ELEV_MAX, ELEV_MAX);
    }

    /// Multiplicative dolly, clamped.
    pub fn dolly(&mut self, factor: f32) {
        self.distance = (self.distance * factor).clamp(DIST_MIN, DIST_MAX);
    }

    /// Pan the target along the camera-plane right/up axes. `dx`/`dy` are
    /// screen-space fractions (⁓pixels normalized), scaled by the current
    /// distance so panning speed feels constant at any zoom.
    pub fn pan(&mut self, dx: f32, dy: f32) {
        let v = view_matrix(self.eye(), self.target);
        // Basis columns: right = x row, up = y row (column-major storage).
        let right = [v[0], v[4], v[8]];
        let up = [v[1], v[5], v[9]];
        let amt = 2.0 * self.distance;
        for k in 0..3 {
            self.target[k] += amt * (dx * right[k] + dy * up[k]);
        }
    }

    /// `F`: restore the default vantage (aspect retained).
    pub fn reset(&mut self) {
        let aspect = self.aspect;
        *self = Self {
            aspect,
            ..Self::default_vantage(1, 1)
        };
    }

    /// `X`/`Y`/`Z`: snap to top/side/front. Snap *into* the clamp (the
    /// top view sits at +85°, not 90°, keeping the up vector healthy).
    pub fn snap(&mut self, view: SnapView) {
        // Keep pan target (the user may have moved it) but restore a
        // sensible distance for the framing.
        self.distance = self.distance.clamp(DIST_MIN, DIST_MAX);
        match view {
            SnapView::Top => {
                self.elevation = ELEV_MAX;
                self.azimuth = 0.0;
            }
            SnapView::Side => {
                self.elevation = 0.0;
                self.azimuth = std::f32::consts::FRAC_PI_2;
            }
            SnapView::Front => {
                self.elevation = 0.0;
                self.azimuth = 0.0;
            }
        }
    }

    /// `--autospin`: continuous slow yaw. Call once per render frame.
    pub fn spin(&mut self, dt_secs: f32) {
        self.azimuth += AUTOSPIN_RATE * dt_secs;
    }
}

/// Right-handed look-at view matrix from `eye` toward `target`, y-up.
pub fn view_matrix(eye: [f32; 3], target: [f32; 3]) -> [f32; 16] {
    // Forward = from eye toward target (negated in view space).
    let z = normalize([
        eye[0] - target[0],
        eye[1] - target[1],
        eye[2] - target[2],
    ]);
    let x = normalize(cross([0.0, 1.0, 0.0], z));
    let y = cross(z, x);
    // View matrix rows are the basis axes of view space in world coords.
    [
        x[0], y[0], z[0], 0.0, x[1], y[1], z[1], 0.0, x[2], y[2], z[2], 0.0, -dot(x, eye),
        -dot(y, eye), -dot(z, eye), 1.0,
    ]
}

/// Perspective projection for wgpu: view-space camera looks down **-z**,
/// NDC depth in **[0, 1]**, y up, x right.
pub fn proj_matrix(cam: &Camera) -> [f32; 16] {
    let f = 1.0 / (cam.fov_y / 2.0).tan();
    // NDC depth in [0, 1] (wgpu): ndc(d) = A·(1 − n/d).
    let a = -cam.far / (cam.far - cam.near);
    let b = a * cam.near;
    // column-major:
    [
        f / cam.aspect, 0.0, 0.0, 0.0, 0.0, f, 0.0, 0.0, 0.0, 0.0, a, -1.0, 0.0, 0.0, b,
        0.0,
    ]
}

pub fn mul(a: [f32; 16], b: [f32; 16]) -> [f32; 16] {
    let mut out = [0.0f32; 16];
    for col in 0..4 {
        for row in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += a[k * 4 + row] * b[col * 4 + k];
            }
            out[col * 4 + row] = s;
        }
    }
    out
}

/// Transform a world-space point by a (column-major) matrix; returns NDC.
pub fn project(m: [f32; 16], p: [f32; 3]) -> [f32; 3] {
    let clip = [p[0], p[1], p[2], 1.0];
    let mut c = [0.0f32; 4];
    for row in 0..4 {
        c[row] = m[row] * clip[0] + m[4 + row] * clip[1] + m[8 + row] * clip[2] + m[12 + row];
    }
    let w = c[3].max(f32::MIN_POSITIVE);
    [c[0] / w, c[1] / w, c[2] / w]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = dot(v, v).sqrt();
    if len > f32::EPSILON {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32, tol: f32) {
        assert!((a - b).abs() < tol, "{a} vs {b}");
    }

    fn base() -> Camera {
        Camera {
            target: [0.0, 0.0, 0.0],
            distance: DISTANCE,
            azimuth: 0.0,
            elevation: ELEVATION,
            fov_y: FOV_Y,
            near: NEAR,
            far: FAR,
            aspect: 1.0,
        }
    }

    #[test]
    fn look_at_target_projects_to_center() {
        let cam = base();
        let vp = cam.view_proj();
        let ndc = project(vp, cam.target);
        assert!(ndc[0].abs() < 1e-4, "x {ndc:?}");
        assert!(ndc[1].abs() < 1e-4, "y {ndc:?}");
        assert!(ndc[2] > 0.0 && ndc[2] < 1.0, "depth {ndc:?}");
    }

    #[test]
    fn spherical_eye_offsets() {
        // θ=90°, φ=0 ⇒ eye = target + (d, 0, 0).
        let mut c = base();
        c.azimuth = std::f32::consts::FRAC_PI_2;
        c.elevation = 0.0;
        approx(c.eye()[0], c.distance, 1e-5);
        approx(c.eye()[1], 0.0, 1e-6);
        approx(c.eye()[2], 0.0, 1e-5);

        // φ=90° (before clamping) would put the eye straight above.
        let mut c = base();
        c.elevation = std::f32::consts::FRAC_PI_2;
        approx(c.eye()[1], c.distance, 1e-5);
        approx(c.eye()[0], 0.0, 1e-5);
        approx(c.eye()[2], 0.0, 1e-5);
    }

    #[test]
    fn orbit_clamps_elevation_not_azimuth() {
        let mut c = base();
        c.orbit(1.0, 10.0); // big dphi
        approx(c.elevation, ELEV_MAX, 1e-6);
        c.orbit(1.0, -50.0);
        approx(c.elevation, -ELEV_MAX, 1e-6);
        // azimuth accumulates (no clamping): 1 + 1 = 2.
        approx(c.azimuth, 2.0, 1e-5);
    }

    #[test]
    fn dolly_clamps() {
        let mut c = base();
        for _ in 0..200 {
            c.dolly(1.2);
        }
        approx(c.distance, DIST_MAX, 1e-10);
        for _ in 0..400 {
            c.dolly(0.8);
        }
        approx(c.distance, DIST_MIN, 1e-10);
        // factor 1.0 is a no-op, exact.
        let before = c.distance;
        c.dolly(1.0);
        assert_eq!(c.distance, before);
    }

    #[test]
    fn pan_moves_along_camera_plane_axes() {
        // Elevation 0, azimuth 0: right = +x, up = +y.
        let mut c = base();
        c.elevation = 0.0;
        c.pan(0.5, 0.25);
        let d = c.distance; // 3.5, amt = 2d = 7
        approx(c.target[0], 2.0 * d * 0.5, 1e-5);
        approx(c.target[1], 2.0 * d * 0.25, 1e-5);
        approx(c.target[2], 0.0, 1e-5);
        c.pan(0.0, 0.25);
        approx(c.target[1], 2.0 * d * 0.5, 1e-5);

        // At azimuth 90° the camera looks down −x; right = −z? Camera
        // right basis: forward = −(eye−target) = −x̂; y-up cross gives
        // right = −ẑ... hand-derive: z_axis = x̂ ⇒ x = cross(ŷ, x̂) = −ẑ.
        let mut s = base();
        s.elevation = 0.0;
        s.azimuth = std::f32::consts::FRAC_PI_2;
        s.pan(0.1, 0.0);
        approx(s.target[0], 0.0, 1e-6);
        approx(s.target[2], -2.0 * d * 0.1, 1e-5);
    }

    #[test]
    fn reset_restores_defaults() {
        let mut c = Camera::default_vantage(800, 600);
        c.orbit(2.0, -1.0);
        c.dolly(4.0);
        c.target = [9.0, 9.0, 9.0];
        c.reset();
        let dflt = Camera::default_vantage(800, 600);
        approx(c.azimuth, dflt.azimuth, 1e-6);
        approx(c.elevation, dflt.elevation, 1e-6);
        approx(c.distance, dflt.distance, 1e-6);
        approx(c.target[0], dflt.target[0], 1e-6);
        // aspect is the *current* one, not the constructor's.
        approx(c.aspect, 800.0 / 600.0, 1e-6);
    }

    #[test]
    fn snap_views() {
        let mut c = base();
        c.orbit(10.0, 3.0);
        c.snap(SnapView::Front);
        approx(c.azimuth, 0.0, 1e-6);
        approx(c.elevation, 0.0, 1e-6);
        c.snap(SnapView::Side);
        approx(c.azimuth, std::f32::consts::FRAC_PI_2, 1e-6);
        approx(c.elevation, 0.0, 1e-6);
        c.snap(SnapView::Top);
        approx(c.azimuth, 0.0, 1e-6);
        approx(c.elevation, ELEV_MAX, 1e-6);
    }

    #[test]
    fn autospin_yaws() {
        let mut c = base();
        let a0 = c.azimuth;
        c.spin(0.5);
        approx(c.azimuth, a0 + AUTOSPIN_RATE * 0.5, 1e-6);
        c.spin(2.0);
        approx(c.azimuth, a0 + AUTOSPIN_RATE * 2.5, 1e-6);
    }

    #[test]
    fn eye_after_drag_stays_on_sphere() {
        let mut c = base();
        c.orbit(0.7, -0.3);
        c.dolly(1.7);
        let [ex, ey, ez] = c.eye();
        // offset from target must have length exactly d.
        let d = ((ex - c.target[0]).powi(2)
            + (ey - c.target[1]).powi(2)
            + (ez - c.target[2]).powi(2))
        .sqrt();
        approx(d, c.distance, 1e-4);
    }
}
