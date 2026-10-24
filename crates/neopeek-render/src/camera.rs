//! Camera (PLAN.md §4.3) — for M4: a *fixed* vantage matching sndpeek's
//! startup look (eye above the origin, slight downward tilt, fov 45°).
//! Orbit/pan/zoom input lands in M5; the math below is shared by both.
//!
//! Minimal hand-rolled mat4 helpers: keeping the dependency set at wgpu-only.
//! Matrices are column-major (WGSL/bfq-style, exactly what
//! `crate::shell` uploads).

/// 3.5 world units from the target — sndpeek's eye distance.
pub const DISTANCE: f32 = 3.5;
/// Elevation of the eye above the horizontal plane (radians, «slight tilt»).
pub const ELEVATION: f32 = 0.55;
pub const FOV_Y: f32 = std::f32::consts::FRAC_PI_4; // fov 45
pub const NEAR: f32 = 0.1;
pub const FAR: f32 = 300.0;

#[derive(Debug, Clone, Copy)]
pub struct Camera {
    /// Look-at point; defaults to the waterfall mid-point.
    pub target: [f32; 3],
    pub distance: f32,
    pub azimuth: f32,
    pub elevation: f32,
    pub fov_y: f32,
    pub near: f32,
    pub far: f32,
    pub aspect: f32,
}

impl Camera {
    /// M4 default vantage: eye above the origin looking slightly down the
    /// waterfall mid-line (which extends toward -z).
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
    /// `eye = target + d·(cosφ·sinθ, sinφ, cosφ·cosθ)`.
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
    // View matrix: rows are the basis axes of view space in world coords.
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
    // a·b: out[col][row] = Σ a[col][k]·b[?][?] with column-major indexing
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

    #[test]
    fn look_at_target_projects_to_center() {
        let cam = Camera {
            target: [0.0, 0.25, -1.3],
            distance: DISTANCE,
            azimuth: 0.0,
            elevation: ELEVATION,
            fov_y: FOV_Y,
            near: NEAR,
            far: FAR,
            aspect: 2.0,
        };
        let vp = cam.view_proj();
        let ndc = project(vp, cam.target);
        assert!(ndc[0].abs() < 1e-4, "x {ndc:?}");
        assert!(ndc[1].abs() < 1e-4, "y {ndc:?}");
        // The target is between near and far: depth inside [0, 1].
        assert!(ndc[2] > 0.0 && ndc[2] < 1.0, "depth {ndc:?}");
    }

    #[test]
    fn eye_sits_above_target() {
        let cam = Camera::default_vantage(800, 600);
        let e = cam.eye();
        assert!(e[1] > cam.target[1], "eye above origin, slight tilt");
        // Camera looks roughly toward -z (azimuth 0 ⇒ eye offset is +z).
        assert!(e[2] > cam.target[2]);
    }

    #[test]
    fn perspective_spreads_x_then_y() {
        let cam = Camera {
            target: [0.0, 0.0, 0.0],
            distance: DISTANCE,
            azimuth: 0.0,
            elevation: 0.0,
            fov_y: FOV_Y,
            near: NEAR,
            far: FAR,
            aspect: 1.0,
        };
        let vp = cam.view_proj();
        let center = project(vp, [0.0, 0.0, 0.0]);
        assert!(center[0].abs() < 1e-6 && center[1].abs() < 1e-6);

        // A point one unit right of the target projects right of center,
        // and its |x| < |step| for y (45° fov ⇒ tan = 1; x step at z=-3.5).
        let right = project(vp, [1.0, 0.0, 0.0]);
        let up = project(vp, [0.0, 1.0, 0.0]);
        assert!(right[0] > 0.0, "x {right:?}");
        assert!(up[1] > 0.0, "y {up:?}");
        // In a symmetric (square-aspect) perspective the unit x and unit y
        // steps land symmetrically.
        assert!((right[0] - up[1]).abs() < 1e-4);
    }

    #[test]
    fn depth_range_endpoints() {
        // Points at near and far distance map to 0 and 1.
        // Eye on the +z axis (elevation 0), looking down −z; probes sit at
        // near/far in front of the eye.
        let cam = Camera {
            target: [0.0, 0.0, 0.0],
            distance: DISTANCE,
            azimuth: 0.0,
            elevation: 0.0,
            fov_y: FOV_Y,
            near: 0.1,
            far: 300.0,
            aspect: 1.0,
        };
        let vp = cam.view_proj();
        // Eye is at +3.5·z (elevation 0); put probes ahead of the eye.
        let near_probe = project(vp, [0.0, 0.0, cam.eye()[2] - cam.near]);
        let far_probe = project(vp, [0.0, 0.0, cam.eye()[2] - cam.far]);
        assert!(near_probe[2].abs() < 1e-3, "near {near_probe:?}");
        assert!(far_probe[2] > 0.999, "far {far_probe:?}");
    }
}
