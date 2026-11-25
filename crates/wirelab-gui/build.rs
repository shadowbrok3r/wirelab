use egui_icon_pack_build::IconPackBuild;

fn main() -> Result<(), egui_icon_pack_build::Error> {
    // The binary embeds the whole assets tree.
    println!("cargo:rerun-if-changed=../../assets");
    IconPackBuild {
        uri_prefix: "wirelab/icons".into(),
        ..IconPackBuild::in_out_dir("../../assets/icons", "icon_table.rs")
    }
    .generate()?;
    Ok(())
}
