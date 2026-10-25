use egui_icon_pack_build::IconPackBuild;

fn main() -> Result<(), egui_icon_pack_build::Error> {
    IconPackBuild {
        uri_prefix: "wirelab/icons".into(),
        ..IconPackBuild::in_out_dir("../../assets/icons", "icon_table.rs")
    }
    .generate()?;
    Ok(())
}
