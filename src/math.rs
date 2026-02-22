// math.rs — Simple 3D vector type used throughout the overlay.

/// A 3-component vector (x, y, z) matching the engine's float[3] layout.
/// Used for world-space positions (player origins, head/feet positions).
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    /// Euclidean distance between two 3D points.
    pub fn distance(self, other: Self) -> f32 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        let dz = self.z - other.z;
        (dx * dx + dy * dy + dz * dz).sqrt()
    }

    /// Check if all components are exactly zero (uninitialized entity).
    pub fn is_zero(self) -> bool {
        self.x == 0.0 && self.y == 0.0 && self.z == 0.0
    }
}
