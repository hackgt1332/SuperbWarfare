//! Deterministically flatten the original Bedrock rest-pose guns into the
//! native 26.3 cuboid item format. This tool never modifies source textures.
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    error::Error,
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
type V3 = [f64; 3];
type M3 = [[f64; 3]; 3];
const FACES: [&str; 6] = ["north", "east", "south", "west", "up", "down"];
// These bones are mutually exclusive upgrades, chamber rounds, or the
// original renderer's arm anchors. The native item renderer supplies hands.
const OMIT: [&str; 9] = [
    "custom_hand_guard",
    "oem_stock_light",
    "oem_stock_heavy",
    "custom_stock_adapter",
    "magazine_extend",
    "magazine_extend_pro",
    "lefthand",
    "righthand",
    "bullet",
];
#[derive(Clone, Copy, Debug)]
struct Transform {
    r: M3,
    t: V3,
}
impl Transform {
    fn rotation(pivot: V3, angles: V3) -> Self {
        let r = rotation(angles);
        Self {
            r,
            t: sub(pivot, mul(r, pivot)),
        }
    }
    fn then(self, child: Self) -> Self {
        Self {
            r: matmul(self.r, child.r),
            t: add(mul(self.r, child.t), self.t),
        }
    }
    fn point(self, point: V3) -> V3 {
        add(mul(self.r, point), self.t)
    }
}
fn add(a: V3, b: V3) -> V3 {
    std::array::from_fn(|i| a[i] + b[i])
}
fn sub(a: V3, b: V3) -> V3 {
    std::array::from_fn(|i| a[i] - b[i])
}
fn mul(m: M3, v: V3) -> V3 {
    m.map(|row| (0..3).map(|i| row[i] * v[i]).sum())
}
fn matmul(a: M3, b: M3) -> M3 {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|i| a[r][i] * b[i][c]).sum()))
}
// The source gun assets use the opposite angle sign from native item
// elements. Preserve positions/face directions, negate each signed angle.
fn rotation(angles: V3) -> M3 {
    native_rotation(angles.map(|v| -v))
}
fn native_rotation([x, y, z]: V3) -> M3 {
    let (sx, cx) = x.to_radians().sin_cos();
    let (sy, cy) = y.to_radians().sin_cos();
    let (sz, cz) = z.to_radians().sin_cos();
    [
        [cz * cy, cz * sy * sx - sz * cx, cz * sy * cx + sz * sx],
        [sz * cy, sz * sy * sx + cz * cx, sz * sy * cx - cz * sx],
        [-sy, cy * sx, cy * cx],
    ]
}
fn euler(m: M3) -> V3 {
    let y = (-m[2][0]).clamp(-1., 1.).asin();
    let (x, z) = if y.cos().abs() > 1e-9 {
        (m[2][1].atan2(m[2][2]), m[1][0].atan2(m[0][0]))
    } else {
        ((-m[1][2]).atan2(m[1][1]), 0.)
    };
    [x, y, z].map(|a| clean(a.to_degrees()))
}
fn clean(v: f64) -> f64 {
    let v = (v * 1e6).round() / 1e6;
    if v == 0. { 0. } else { v }
}
fn vec3(value: &Value) -> Result<V3> {
    let a = value.as_array().ok_or("expected vector")?;
    if a.len() != 3 {
        return Err("expected three coordinates".into());
    }
    let mut result = [0.; 3];
    for (i, v) in a.iter().enumerate() {
        result[i] = v
            .as_f64()
            .filter(|v| v.is_finite())
            .ok_or("non-finite coordinate")?;
    }
    Ok(result)
}
fn optional_vec(value: &Value, name: &str) -> Result<V3> {
    value.get(name).map(vec3).unwrap_or(Ok([0.; 3]))
}
fn omit(name: &str, bones: &BTreeMap<&str, &Value>, visiting: &mut Vec<String>) -> Result<bool> {
    if OMIT.contains(&name) {
        return Ok(true);
    }
    if visiting.iter().any(|n| n == name) {
        return Err("bone parent cycle".into());
    }
    visiting.push(name.into());
    let bone = bones.get(name).ok_or("missing bone")?;
    let hidden = if let Some(parent) = bone["parent"].as_str() {
        omit(parent, bones, visiting)?
    } else {
        false
    };
    visiting.pop();
    Ok(hidden)
}
fn bone_transform(
    name: &str,
    bones: &BTreeMap<&str, &Value>,
    visiting: &mut Vec<String>,
) -> Result<Transform> {
    if visiting.iter().any(|n| n == name) {
        return Err("bone parent cycle".into());
    }
    visiting.push(name.into());
    let bone = bones.get(name).ok_or("missing bone")?;
    let own = Transform::rotation(
        optional_vec(bone, "pivot")?,
        optional_vec(bone, "rotation")?,
    );
    let result = if let Some(parent) = bone["parent"].as_str() {
        bone_transform(parent, bones, visiting)?.then(own)
    } else {
        own
    };
    visiting.pop();
    Ok(result)
}
fn face_uv(face: &Value, side: &str, texture: [f64; 2]) -> Result<[f64; 4]> {
    let uv = face["uv"].as_array().ok_or("missing per-face UV")?;
    let size = face["uv_size"]
        .as_array()
        .ok_or("missing explicit per-face UV size")?;
    if uv.len() != 2 || size.len() != 2 {
        return Err("invalid face UV shape".into());
    }
    let n = |v: &Value| v.as_f64().filter(|f| f.is_finite()).ok_or("non-finite UV");
    let (u, v, du, dv) = (n(&uv[0])?, n(&uv[1])?, n(&size[0])?, n(&size[1])?);
    // Bedrock's top origin is back-left, bottom origin back-right. The
    // native cuboid order starts top-front-left and bottom-back-left.
    let out = match side {
        "up" => [u, v + dv, u + du, v],
        "down" => [u + du, v, u, v + dv],
        _ => [u, v, u + du, v + dv],
    };
    Ok(std::array::from_fn(|i| {
        clean(out[i] * 16. / texture[i % 2])
    }))
}
fn display(name: &str) -> Value {
    let ak = name == "ak_47";
    let first = json!({"rotation":[0,0,0],"translation":if ak {[-3.6,2.6,0.]} else {[-4.5,3.1,3.]},"scale":if ak {[0.65;3]} else {[0.5;3]}});
    let third = json!({"rotation":[0,0,0],"translation":if ak {[0.,-0.27,-0.46]} else {[0.,-0.15,0.04]},"scale":if ak {[0.55;3]} else {[0.35;3]}});
    json!({"firstperson_righthand":first,"firstperson_lefthand":first,
        "thirdperson_righthand":third,"thirdperson_lefthand":third,
        "gui":{"rotation":[20,-45,0],"translation":if ak {[0.,-1.5,0.]} else {[0.,0.8,0.]},"scale":if ak {[0.5;3]} else {[0.95;3]}},
        "ground":{"rotation":[0,0,90],"translation":[0,1.5,0],"scale":if ak {[0.45;3]} else {[0.6;3]}},
        "fixed":{"rotation":[0,90,0],"translation":[0,0,0],"scale":if ak {[0.5;3]} else {[1.;3]}}})
}
fn convert(source: &Value, name: &str) -> Result<Value> {
    let geometry = source["minecraft:geometry"]
        .as_array()
        .and_then(|v| v.first())
        .ok_or("missing geometry")?;
    let bones_list = geometry["bones"].as_array().ok_or("missing bones")?;
    let mut bones = BTreeMap::new();
    for bone in bones_list {
        let name = bone["name"].as_str().ok_or("missing bone name")?;
        if bones.insert(name, bone).is_some() {
            return Err("duplicate bone name".into());
        }
    }
    let texture = [
        geometry["description"]["texture_width"]
            .as_f64()
            .ok_or("missing texture width")?,
        geometry["description"]["texture_height"]
            .as_f64()
            .ok_or("missing texture height")?,
    ];
    if texture.iter().any(|v| !v.is_finite() || *v <= 0.) {
        return Err("invalid texture dimensions".into());
    }
    let factor = if name == "ak_47" { 0.5 } else { 1. };
    let mut elements = Vec::new();
    for bone in bones_list {
        let bone_name = bone["name"].as_str().ok_or("missing bone name")?;
        if omit(bone_name, &bones, &mut vec![])? {
            continue;
        }
        if bone["mirror"].as_bool() == Some(true) {
            return Err("mirrored bones need explicit conversion".into());
        }
        let parent = bone_transform(bone_name, &bones, &mut vec![])?;
        for cube in bone["cubes"].as_array().into_iter().flatten() {
            if cube["mirror"].as_bool() == Some(true) {
                return Err("mirrored cubes need explicit conversion".into());
            }
            let origin = vec3(&cube["origin"])?;
            let size = vec3(&cube["size"])?;
            let inflate = cube["inflate"]
                .as_f64()
                .or_else(|| bone["inflate"].as_f64())
                .unwrap_or(0.);
            let half = size.map(|v| v / 2. + inflate);
            if !inflate.is_finite() || half.iter().any(|v| *v < 0.) {
                return Err("invalid cube inflation".into());
            }
            let center = add(origin, size.map(|v| v / 2.));
            let pivot = cube.get("pivot").map(vec3).transpose()?.unwrap_or(center);
            let pose = parent.then(Transform::rotation(pivot, optional_vec(cube, "rotation")?));
            let center = pose.point(center).map(|v| v * factor + 8.);
            let from: V3 = std::array::from_fn(|i| clean(center[i] - half[i] * factor));
            let to: V3 = std::array::from_fn(|i| clean(center[i] + half[i] * factor));
            if from.iter().chain(&to).any(|v| !(-16. ..=32.).contains(v)) {
                return Err(format!("{name}/{bone_name}: cube exceeds native bounds").into());
            }
            let mut faces = serde_json::Map::new();
            for side in FACES {
                if let Some(face) = cube["uv"].get(side) {
                    faces.insert(
                        side.into(),
                        json!({"uv":face_uv(face,side,texture)?,"texture":"#gun"}),
                    );
                }
            }
            let angles = euler(pose.r);
            let mut element = json!({"name":bone_name,"from":from,"to":to,"faces":faces});
            if angles.iter().any(|v| v.abs() > 1e-6) {
                element["rotation"] = json!({"origin":center.map(clean),"x":angles[0],"y":angles[1],"z":angles[2],"rescale":false});
            }
            if bone_name.ends_with("_illuminated") {
                element["light_emission"] = json!(15);
            }
            elements.push(element);
        }
    }
    if elements.is_empty() {
        return Err("empty converted gun".into());
    }
    Ok(
        json!({"credit":"Generated from original SuperbWarfare Bedrock geometry by tools/src/bin/convert-guns.rs; original asset license applies.",
        "ambientocclusion":false,"gui_light":"side","textures":{"gun":format!("superbwarfare:item/{name}_model"),"particle":format!("superbwarfare:item/{name}_model")},
        "display":display(name),"elements":elements}),
    )
}
fn model_bytes(source: &Value, name: &str) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(&convert(source, name)?)?;
    bytes.push(b'\n');
    Ok(bytes)
}
fn input(root: &Path, name: &str) -> PathBuf {
    root.join(format!(
        "src/main/resources/assets/superbwarfare/models/bedrock/gun/{name}.geo.json"
    ))
}
fn output(root: &Path, name: &str) -> PathBuf {
    root.join(format!(
        "rust/horizon-bridge/client/assets/superbwarfare/models/item/{name}.json"
    ))
}
fn produce(root: &Path, check: bool) -> Result<()> {
    for name in ["ak_47", "glock_17"] {
        let source = serde_json::from_slice(&std::fs::read(input(root, name))?)?;
        let model = model_bytes(&source, name)?;
        let src_texture = root.join(format!(
            "src/main/resources/assets/superbwarfare/textures/bedrock/gun/{name}.png"
        ));
        let dst_texture = root.join(format!(
            "rust/horizon-bridge/client/assets/superbwarfare/textures/item/{name}_model.png"
        ));
        let texture = std::fs::read(&src_texture)?;
        if check {
            if std::fs::read(output(root, name))? != model
                || std::fs::read(&dst_texture)? != texture
            {
                return Err(format!("{name}: generated assets are stale; run convert-guns").into());
            }
        } else {
            std::fs::write(output(root, name), &model)?;
            std::fs::write(dst_texture, &texture)?;
        }
        println!(
            "{name}: {} cuboids, {} model bytes",
            source_count(&source, name)?,
            model.len()
        );
    }
    Ok(())
}
fn source_count(source: &Value, name: &str) -> Result<usize> {
    Ok(convert(source, name)?["elements"]
        .as_array()
        .ok_or("missing converted elements")?
        .len())
}
fn main() -> Result<()> {
    let mut check = false;
    let mut root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    for arg in std::env::args().skip(1) {
        if arg == "--check" {
            check = true;
        } else {
            root = PathBuf::from(arg);
        }
    }
    produce(&root, check)
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_assets::{
        bake::ElementMatrix,
        model::{CuboidModel, ElementRotation, RotationValue},
    };
    #[test]
    fn flattened_rotation_matches_the_native_element_matrix() {
        for (a, b) in [
            ([22.5, -17., 35.], [6., 8.5, -12.]),
            ([0., 90., 12.], [0.; 3]),
            ([0., -90., 12.], [0.; 3]),
        ] {
            let expected = matmul(rotation(a), rotation(b));
            let result = euler(expected);
            let native = ElementMatrix::from_rotation(&ElementRotation {
                origin: [0.; 3],
                value: RotationValue::EulerXyz(result.map(|v| v as f32)),
                rescale: false,
            });
            for (r, row) in expected.iter().enumerate() {
                for (c, v) in row.iter().enumerate() {
                    assert!((native.rows[r][c] as f64 - v).abs() < 0.0001);
                }
            }
        }
    }
    #[test]
    fn nested_pivots_keep_cube_corners_in_their_original_rest_pose() {
        let parent = Transform::rotation([1., 3., 5.], [17., 5., -35.]);
        let full = parent.then(Transform::rotation([-2., 8., -1.], [3., 41., 0.]));
        let center = [1., 2., 3.];
        let shifted = full.point(center);
        for delta in [[-1., -2., -3.], [1., 2., 3.]] {
            let flattened = add(mul(native_rotation(euler(full.r)), delta), shifted);
            let original = full.point(add(center, delta));
            for i in 0..3 {
                assert!((flattened[i] - original[i]).abs() < 1e-6);
            }
        }
    }
    #[test]
    fn generated_guns_parse_with_native_models_and_keep_source_textures() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        for (name, count) in [("ak_47", 372), ("glock_17", 195)] {
            let source =
                serde_json::from_slice(&std::fs::read(input(&root, name)).unwrap()).unwrap();
            let bytes = model_bytes(&source, name).unwrap();
            let model = CuboidModel::from_json(std::str::from_utf8(&bytes).unwrap()).unwrap();
            assert_eq!(model.elements.as_ref().unwrap().len(), count);
            assert!(model.display.is_some());
            struct Texture;
            impl horizon_assets::item_bake::SpriteLookup for Texture {
                fn mask(&self, _: &str) -> Option<horizon_assets::item_bake::SpriteMask> {
                    Some(horizon_assets::item_bake::SpriteMask::opaque(1, 1))
                }
            }
            let id = format!("superbwarfare:item/{name}");
            let library =
                horizon_assets::resolve::ModelLibrary::new(BTreeMap::from([(id.clone(), model)]))
                    .unwrap();
            let baked =
                horizon_assets::item_bake::bake_item_model(&library, &Texture, &id).unwrap();
            assert!(baked.quads.len() > count * 3);
            assert!(baked.quads.iter().all(
                |q| q.positions.iter().flatten().all(|v| v.is_finite())
                    && q.uv.iter().flatten().all(|v| v.is_finite())
                    && q.material.sprite == format!("superbwarfare:item/{name}_model")
            ));
            assert!(baked.transforms[..4].iter().all(Option::is_some));
            assert_eq!(std::fs::read(output(&root, name)).unwrap(), bytes);
            let src = std::fs::read(root.join(format!(
                "src/main/resources/assets/superbwarfare/textures/bedrock/gun/{name}.png"
            )))
            .unwrap();
            let dst = std::fs::read(root.join(format!(
                "rust/horizon-bridge/client/assets/superbwarfare/textures/item/{name}_model.png"
            )))
            .unwrap();
            assert_eq!(src, dst);
            assert_eq!(&dst[..8], b"\x89PNG\r\n\x1a\n");
        }
    }
    #[test]
    fn original_glock_rotation_keeps_its_frame_detail_on_the_body() {
        // A signed -22.5 degree source cube is pre-positioned outside the
        // frame. The native positive rotation returns it to the right side;
        // using the source angle directly leaves an obvious detached shard.
        let pose = Transform::rotation([0.08296, -3.05902, 0.58585], [0., 0., -22.5]);
        let center = [
            2.13903 + 0.09766 / 2.,
            0.151 + 0.33203 / 2.,
            1.3217 + 0.52734 / 2.,
        ];
        let transformed = pose.point(center);
        assert!((transformed[0] - 0.73558).abs() < 0.001);
        assert!((transformed[1] - 0.86559).abs() < 0.001);
    }
    #[test]
    fn opposite_face_uv_origins_are_preserved() {
        let f = json!({"uv":[8,16],"uv_size":[4,-2]});
        assert_eq!(
            face_uv(&f, "north", [128., 128.]).unwrap(),
            [1., 2., 1.5, 1.75]
        );
        assert_eq!(
            face_uv(&f, "up", [128., 128.]).unwrap(),
            [1., 1.75, 1.5, 2.]
        );
        assert_eq!(
            face_uv(&f, "down", [128., 128.]).unwrap(),
            [1.5, 2., 1., 1.75]
        );
    }
}
