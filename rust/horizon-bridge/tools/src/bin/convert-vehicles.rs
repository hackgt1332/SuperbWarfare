//! Development-only asset conversion. Player packages contain native model JSON,
//! original textures and Rust/WASM; this converter is not loaded by the game.
use serde_json::{Value, json};
use std::{collections::BTreeMap, error::Error, path::Path};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
type V = [f64; 3];
fn add(a: V, b: V) -> V {
    std::array::from_fn(|i| a[i] + b[i])
}
fn sub(a: V, b: V) -> V {
    std::array::from_fn(|i| a[i] - b[i])
}
fn mul(a: V, n: f64) -> V {
    a.map(|x| x * n)
}
fn vec(v: &Value) -> V {
    std::array::from_fn(|i| v[i].as_f64().unwrap_or(0.))
}
fn rotate(mut p: V, r: V) -> V {
    let (s, c) = (-r[0]).to_radians().sin_cos();
    p = [p[0], c * p[1] - s * p[2], s * p[1] + c * p[2]];
    let (s, c) = (-r[1]).to_radians().sin_cos();
    p = [c * p[0] + s * p[2], p[1], -s * p[0] + c * p[2]];
    let (s, c) = (-r[2]).to_radians().sin_cos();
    [c * p[0] - s * p[1], s * p[0] + c * p[1], p[2]]
}
fn bone_point(mut p: V, name: &str, bones: &BTreeMap<String, &Value>) -> V {
    let mut current = Some(name);
    let mut budget = bones.len() + 1;
    while let Some(name) = current {
        assert!(budget > 0, "cyclic model bones");
        budget -= 1;
        let bone = bones[name];
        let pivot = vec(&bone["pivot"]);
        p = add(rotate(sub(p, pivot), vec(&bone["rotation"])), pivot);
        current = bone["parent"].as_str();
    }
    p
}
fn group<'a>(mut name: &'a str, bones: &'a BTreeMap<String, &Value>) -> Option<&'a str> {
    let mut wheel = None;
    loop {
        if ["icon", "senpai", "chest"].contains(&name) {
            return None;
        }
        if name.starts_with("wheel") {
            wheel = Some(name);
        }
        let Some(parent) = bones[name]["parent"].as_str() else {
            break;
        };
        name = parent;
    }
    Some(wheel.unwrap_or("body"))
}
const FACES: [(&str, [usize; 4]); 6] = [
    ("north", [3, 2, 0, 1]),
    ("south", [6, 7, 5, 4]),
    ("west", [2, 6, 4, 0]),
    ("east", [7, 3, 1, 5]),
    ("up", [2, 3, 7, 6]),
    ("down", [4, 5, 1, 0]),
];
fn convert(source: &Value, name: &str, length: f64) -> Result<Value> {
    let geo = &source["minecraft:geometry"][0];
    let bones: BTreeMap<_, _> = geo["bones"]
        .as_array()
        .ok_or("bones missing")?
        .iter()
        .map(|b| (b["name"].as_str().unwrap().to_string(), b))
        .collect();
    let texture = [
        geo["description"]["texture_width"].as_f64().unwrap(),
        geo["description"]["texture_height"].as_f64().unwrap(),
    ];
    let mut polygons: BTreeMap<String, Vec<Vec<[f64; 5]>>> = BTreeMap::new();
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for (bone_name, bone) in &bones {
        let Some(group) = group(bone_name, &bones) else {
            continue;
        };
        for cube in bone["cubes"].as_array().into_iter().flatten() {
            let origin = vec(&cube["origin"]);
            let size = vec(&cube["size"]);
            let inflate = cube["inflate"].as_f64().unwrap_or(0.);
            let pivot = cube.get("pivot").map(vec).unwrap_or([0.; 3]);
            let rotation = vec(&cube["rotation"]);
            let points: [V; 8] = std::array::from_fn(|i| {
                let p = std::array::from_fn(|a| {
                    origin[a]
                        + if i & (1 << a) == 0 {
                            -inflate * size[a].signum()
                        } else {
                            size[a] + inflate * size[a].signum()
                        }
                });
                let p = bone_point(
                    add(rotate(sub(p, pivot), rotation), pivot),
                    bone_name,
                    &bones,
                );
                [-p[0], p[1], -p[2]] // Original models point -Z; native chassis forward is +Z.
            });
            for (face, indices) in FACES {
                let Some(uv) = cube["uv"].get(face) else {
                    continue;
                };
                let u = uv["uv"][0].as_f64().unwrap();
                let v = uv["uv"][1].as_f64().unwrap();
                let du = uv["uv_size"][0].as_f64().unwrap();
                let dv = uv["uv_size"][1].as_f64().unwrap();
                let uv = [[u, v], [u + du, v], [u + du, v + dv], [u, v + dv]];
                let a = sub(points[indices[1]], points[indices[0]]);
                let b = sub(points[indices[2]], points[indices[0]]);
                let cross = [
                    a[1] * b[2] - a[2] * b[1],
                    a[2] * b[0] - a[0] * b[2],
                    a[0] * b[1] - a[1] * b[0],
                ];
                if cross.iter().map(|n| n * n).sum::<f64>() < 1e-14 {
                    continue;
                }
                let vertices = (0..4)
                    .map(|i| {
                        let p = points[indices[i]];
                        for a in 0..3 {
                            lo[a] = lo[a].min(p[a]);
                            hi[a] = hi[a].max(p[a]);
                        }
                        [
                            p[0],
                            p[1],
                            p[2],
                            uv[i][0] / texture[0],
                            uv[i][1] / texture[1],
                        ]
                    })
                    .collect();
                polygons.entry(group.into()).or_default().push(vertices);
            }
        }
    }
    let scale = length * 16. / (hi[2] - lo[2]);
    let center = [(lo[0] + hi[0]) / 2., lo[1], (lo[2] + hi[2]) / 2.];
    let transform = |p: V| mul(sub(p, center), scale);
    let mut children = serde_json::Map::new();
    let mut wheel_parts = vec![];
    let mut steer_parts = vec![];
    for (part, faces) in polygons {
        let pivot = if part == "body" {
            [0.; 3]
        } else {
            let p = bone_point(vec(&bones[&part]["pivot"]), &part, &bones);
            [-p[0], p[1], -p[2]]
        };
        let at = transform(pivot);
        let faces: Vec<_> = faces
            .into_iter()
            .map(|p| {
                p.into_iter()
                    .map(|p| {
                        let v = mul(sub([p[0], p[1], p[2]], pivot), scale);
                        [v[0], v[1], v[2], p[3], p[4]]
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        if part.starts_with("wheel") {
            wheel_parts.push(part.clone());
            if part.ends_with("Turn") {
                steer_parts.push(part.clone());
            }
        }
        children.insert(
            part,
            json!({"pose":[at[0],at[1],at[2],0,0,0,1,1,1],"polygons":faces}),
        );
    }
    Ok(
        json!({"credit":"Converted original SuperbWarfare Bedrock model; original asset license applies.","texture":format!("superbwarfare:entity/vehicle/{name}"),"wheel_parts":wheel_parts,"steer_parts":steer_parts,"wheel_radius":0.35,"bounds":[(hi[0]-lo[0])*scale/16.,(hi[1]-lo[1])*scale/16.,length],"model":{"children":children}}),
    )
}
fn main() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let check = std::env::args().any(|a| a == "--check");
    for (name, length) in [("sodayo_pick_up", 4.7), ("truck", 8.8)] {
        let source = root.join(format!(
            "src/main/resources/assets/superbwarfare/models/bedrock/vehicle/{name}.geo.json"
        ));
        let value = convert(
            &serde_json::from_slice(&std::fs::read(source)?)?,
            name,
            length,
        )?;
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        let output = root.join(format!(
            "rust/horizon-bridge/client/assets/superbwarfare/models/entity/{name}.json"
        ));
        let texture = std::fs::read(root.join(format!(
            "src/main/resources/assets/superbwarfare/textures/bedrock/vehicle/{name}.png"
        )))?;
        let texture_path = root.join(format!(
            "rust/horizon-bridge/client/assets/superbwarfare/textures/entity/vehicle/{name}.png"
        ));
        if check {
            assert_eq!(std::fs::read(&output)?, bytes);
            assert_eq!(std::fs::read(texture_path)?, texture);
        } else {
            std::fs::create_dir_all(output.parent().unwrap())?;
            std::fs::write(output, bytes)?;
            std::fs::create_dir_all(texture_path.parent().unwrap())?;
            std::fs::write(texture_path, texture)?;
        }
        println!("{name}: bounds {}", value["bounds"]);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_models_have_native_quads_and_wheel_groups() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        for (name, length) in [("sodayo_pick_up", 4.7), ("truck", 8.8)] {
            let path = root.join(format!(
                "src/main/resources/assets/superbwarfare/models/bedrock/vehicle/{name}.geo.json"
            ));
            let model = convert(
                &serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap(),
                name,
                length,
            )
            .unwrap();
            assert!(model["wheel_parts"].as_array().unwrap().len() >= 4);
            assert_eq!(model["bounds"][2], length);
            let children = model["model"]["children"].as_object().unwrap();
            assert!(!children.contains_key("senpai"));
            let count: usize = children
                .values()
                .map(|p| {
                    p["polygons"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|p| {
                            assert_eq!(p.as_array().unwrap().len(), 4);
                            1
                        })
                        .sum::<usize>()
                })
                .sum();
            assert!(count > 1000);
        }
    }
}
