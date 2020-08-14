use std::error::Error;
use std::path::Path;
use std::rc::Rc;

#[derive(Clone)]
pub enum CargoTargetType {
    Executable,
    Library {
        has_staticlib: bool,
        has_cdylib: bool,
    },
}

#[derive(Clone)]
pub struct CargoTarget {
    cargo_package: Rc<cargo_metadata::Package>,
    cargo_target: cargo_metadata::Target,
    target_type: CargoTargetType,
}

impl CargoTarget {
    pub fn from_metadata(
        cargo_package: Rc<cargo_metadata::Package>,
        cargo_target: cargo_metadata::Target,
    ) -> Option<Self> {
        let target_type = if cargo_target
            .kind
            .iter()
            .any(|k| k == "staticlib" || k == "cdylib")
        {
            CargoTargetType::Library {
                has_staticlib: cargo_target.kind.iter().any(|k| k == "staticlib"),
                has_cdylib: cargo_target.kind.iter().any(|k| k == "cdylib"),
            }
        } else if cargo_target.kind.iter().any(|k| k == "bin") {
            CargoTargetType::Executable
        } else {
            return None;
        };

        Some(Self {
            cargo_package: cargo_package.clone(),
            cargo_target,
            target_type,
        })
    }

    fn lib_name(&self) -> String {
        self.cargo_target.name.replace("-", "_")
    }

    fn static_lib_name(&self, platform: &super::platform::Platform) -> String {
        if platform.is_msvc() {
            format!("{}.lib", self.lib_name())
        } else {
            format!("lib{}.a", self.lib_name())
        }
    }

    fn dynamic_lib_name(&self, platform: &super::platform::Platform) -> String {
        if platform.is_windows() {
            format!("{}.dll", self.lib_name())
        } else if platform.is_macos() {
            format!("lib{}.dylib", self.lib_name())
        } else {
            format!("lib{}.so", self.lib_name())
        }
    }

    fn implib_name(&self, platform: &super::platform::Platform) -> String {
        let suffix = if platform.is_msvc() {
            "lib"
        } else if platform.is_windows_gnu() {
            "a"
        } else {
            ""
        };

        format!("{}.dll.{}", self.lib_name(), suffix)
    }

    fn exe_name(&self, platform: &super::platform::Platform) -> String {
        if platform.is_windows() {
            format!("{}.exe", self.cargo_target.name)
        } else {
            self.cargo_target.name.clone()
        }
    }

    pub fn emit_cmake_target(
        &self,
        out_file: &mut dyn std::io::Write,
        platform: &super::platform::Platform,
    ) -> Result<(), Box<dyn Error>> {
        // This bit aggregates the byproducts of "cargo build", which is needed for generators like Ninja.
        let mut byproducts = vec![];
        match self.target_type {
            CargoTargetType::Library {
                has_staticlib,
                has_cdylib,
            } => {
                if has_staticlib {
                    byproducts.push(self.static_lib_name(platform));
                }

                if has_cdylib {
                    byproducts.push(self.dynamic_lib_name(platform));

                    if platform.is_windows() {
                        byproducts.push(self.implib_name(platform));
                    }
                }
            }
            CargoTargetType::Executable => {
                byproducts.push(self.exe_name(platform));
            }
        }

        writeln!(
            out_file,
            "\
_add_cargo_build(
    PACKAGE {0}
    TARGET {1}
    MANIFEST_PATH \"{2}\"
    BYPRODUCTS {3}
)
",
            self.cargo_package.name,
            self.cargo_target.name,
            self.cargo_package
                .manifest_path
                .to_str()
                .unwrap()
                .replace("\\", "/"),
            byproducts.join(" ")
        )?;

        match self.target_type {
            CargoTargetType::Library {
                has_staticlib,
                has_cdylib,
            } => {
                assert!(has_staticlib || has_cdylib);

                if has_staticlib {
                    writeln!(
                        out_file,
                        "add_library({0}-static STATIC IMPORTED GLOBAL)\n\
                         add_dependencies({0}-static cargo-build_{0})",
                        self.cargo_target.name
                    )?;

                    if !platform.libs.is_empty() {
                        writeln!(
                            out_file,
                            "set_property(TARGET {0}-static PROPERTY INTERFACE_LINK_LIBRARIES \
                             {1})",
                            self.cargo_target.name,
                            platform.libs.join(" ")
                        )?;
                    }

                    if !platform.libs_debug.is_empty() {
                        writeln!(
                            out_file,
                            "set_property(TARGET {0}-static PROPERTY \
                             INTERFACE_LINK_LIBRARIES_DEBUG {1})",
                            self.cargo_target.name,
                            platform.libs_debug.join(" ")
                        )?;
                    }

                    if !platform.libs_release.is_empty() {
                        for config in &["RELEASE", "MINSIZEREL", "RELWITHDEBINFO"] {
                            writeln!(
                                out_file,
                                "set_property(TARGET {0}-static PROPERTY \
                                 INTERFACE_LINK_LIBRARIES_{2} {1})",
                                self.cargo_target.name,
                                platform.libs_release.join(" "),
                                config
                            )?;
                        }
                    }
                }

                if has_cdylib {
                    writeln!(
                        out_file,
                        "add_library({0}-shared SHARED IMPORTED GLOBAL)\n\
                         add_dependencies({0}-shared cargo-build_{0})",
                        self.cargo_target.name
                    )?;
                }

                writeln!(
                    out_file,
                    "add_library({0} INTERFACE)",
                    self.cargo_target.name
                )?;

                if has_cdylib && has_staticlib {
                    writeln!(
                        out_file,
                        "\
if (BUILD_SHARED_LIBS)
    target_link_libraries({0} INTERFACE {0}-shared)
else()
    target_link_libraries({0} INTERFACE {0}-static)
endif()",
                        self.cargo_target.name
                    )?;
                } else if has_cdylib {
                    writeln!(
                        out_file,
                        "target_link_libraries({0} INTERFACE {0}-shared)",
                        self.cargo_target.name
                    )?;
                } else {
                    writeln!(
                        out_file,
                        "target_link_libraries({0} INTERFACE {0}-static)",
                        self.cargo_target.name
                    )?;
                }
            }
            CargoTargetType::Executable => {
                writeln!(
                    out_file,
                    "add_executable({0} IMPORTED GLOBAL)\n\
                     add_dependencies({0} cargo-build_{0})",
                    self.cargo_target.name
                )?;
            }
        }

        writeln!(out_file)?;

        Ok(())
    }

    pub fn emit_cmake_config_info(
        &self,
        out_file: &mut dyn std::io::Write,
        platform: &super::platform::Platform,
        build_path: &Path,
        config_type: &Option<&str>,
    ) -> Result<(), Box<dyn Error>> {
        let imported_location = config_type.map_or("IMPORTED_LOCATION".to_owned(), |config_type| {
            format!("IMPORTED_LOCATION_{}", config_type.to_uppercase())
        });

        match self.target_type {
            CargoTargetType::Library {
                has_staticlib,
                has_cdylib,
            } => {
                if has_staticlib {
                    let lib_path = build_path
                        .join(self.static_lib_name(platform))
                        .to_str()
                        .unwrap()
                        .replace("\\", "/");

                    writeln!(
                        out_file,
                        "set_property(TARGET {0}-static PROPERTY {1} \"{2}\")",
                        self.cargo_target.name, imported_location, lib_path
                    )?;
                }

                if has_cdylib {
                    let lib_path = build_path
                        .join(self.dynamic_lib_name(platform))
                        .to_str()
                        .unwrap()
                        .replace("\\", "/");

                    writeln!(
                        out_file,
                        "set_property(TARGET {0}-shared PROPERTY {1} \"{2}\")",
                        self.cargo_target.name, imported_location, lib_path
                    )?;

                    if platform.is_windows() {
                        if platform.is_windows_gnu() {
                            println!(
                                "WARNING: Shared libraries from *-pc-windows-gnu cannot be imported"
                            )
                        } else if platform.is_msvc() {
                            let imported_implib =
                                config_type.map_or("IMPORTED_IMPLIB".to_owned(), |config_type| {
                                    format!("IMPORTED_IMPLIB_{}", config_type.to_uppercase())
                                });

                            let lib_path = build_path
                                .join(self.implib_name(platform))
                                .to_str()
                                .unwrap()
                                .replace("\\", "/");

                            writeln!(
                                out_file,
                                "set_property(TARGET {0}-shared PROPERTY {1} \"{2}\")",
                                self.cargo_target.name, imported_implib, lib_path
                            )?;
                        }
                    }
                }
            }
            CargoTargetType::Executable => {
                let exe_file = if platform.is_windows() {
                    format!("{}.exe", self.cargo_target.name)
                } else {
                    self.cargo_target.name.clone()
                };

                let exe_path = build_path
                    .join(exe_file)
                    .to_str()
                    .unwrap()
                    .replace("\\", "/");

                writeln!(
                    out_file,
                    "set_property(TARGET {0} PROPERTY {1} \"{2}\")",
                    self.cargo_target.name, imported_location, exe_path
                )?;
            }
        }

        Ok(())
    }
}
