use regex::Regex;
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::process::Command;

struct PythonVersion {
    major: u8,
    // minor == None means any minor version will do
    minor: Option<u8>,
}

impl fmt::Display for PythonVersion {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        self.major.fmt(f)?;
        f.write_str(".")?;
        match self.minor {
            Some(minor) => minor.fmt(f)?,
            None => f.write_str("*")?,
        };
        Ok(())
    }
}

const CFG_KEY: &str = "py_sys_config";

// windows' python writes out lines with the windows crlf sequence;
// posix platforms and mac os should write out lines with just lf.
#[cfg(target_os = "windows")]
static NEWLINE_SEQUENCE: &str = "\r\n";

#[cfg(not(target_os = "windows"))]
static NEWLINE_SEQUENCE: &str = "\n";

// A list of python interpreter compile-time preprocessor defines that
// we will pick up and pass to rustc via --cfg=py_sys_config={varname};
// this allows using them conditional cfg attributes in the .rs files, so
//
// #[cfg(py_sys_config="{varname}"]
//
// is the equivalent of #ifdef {varname} name in C.
//
// see Misc/SpecialBuilds.txt in the python source for what these mean.
//
// (hrm, this is sort of re-implementing what distutils does, except
// by passing command line args instead of referring to a python.h)
#[cfg(not(target_os = "windows"))]
static SYSCONFIG_FLAGS: [&str; 7] = [
    "Py_USING_UNICODE",
    "Py_UNICODE_WIDE",
    "WITH_THREAD",
    "Py_DEBUG",
    "Py_REF_DEBUG",
    "Py_TRACE_REFS",
    "COUNT_ALLOCS",
];

static SYSCONFIG_VALUES: [&str; 1] = [
    // cfg doesn't support flags with values, just bools - so flags
    // below are translated into bools as {varname}_{val}
    //
    // for example, Py_UNICODE_SIZE_2 or Py_UNICODE_SIZE_4
    "Py_UNICODE_SIZE", // note - not present on python 3.3+, which is always wide
];

/// Examine python's compile flags to pass to cfg by launching
/// the interpreter and printing variables of interest from
/// sysconfig.get_config_vars.
#[cfg(not(target_os = "windows"))]
fn get_config_vars(python_path: &str) -> Result<HashMap<String, String>, String> {
    let mut script = "import sysconfig; \
                      config = sysconfig.get_config_vars();"
        .to_owned();

    for k in SYSCONFIG_FLAGS.iter().chain(SYSCONFIG_VALUES.iter()) {
        script.push_str(&format!(
            "print(config.get('{}', {}))",
            k,
            if is_value(k) { "None" } else { "0" }
        ));
        script.push(';');
    }

    let mut cmd = Command::new(python_path);
    cmd.arg("-c").arg(script);

    let out = cmd
        .output()
        .map_err(|e| format!("failed to run python interpreter `{:?}`: {}", cmd, e))?;

    if !out.status.success() {
        let stderr = String::from_utf8(out.stderr).unwrap();
        let mut msg = "python script failed with stderr:\n\n".to_string();
        msg.push_str(&stderr);
        return Err(msg);
    }

    let stdout = String::from_utf8(out.stdout).unwrap();
    let split_stdout: Vec<&str> = stdout.trim_end().split(NEWLINE_SEQUENCE).collect();
    if split_stdout.len() != SYSCONFIG_VALUES.len() + SYSCONFIG_FLAGS.len() {
        return Err(format!(
            "python stdout len didn't return expected number of lines:
{}",
            split_stdout.len()
        ));
    }
    let all_vars = SYSCONFIG_FLAGS.iter().chain(SYSCONFIG_VALUES.iter());
    // let var_map: HashMap<String, String> = HashMap::new();
    Ok(all_vars.zip(split_stdout.iter()).fold(
        HashMap::new(),
        |mut memo: HashMap<String, String>, (&k, &v)| {
            if !(v == "None" && is_value(k)) {
                memo.insert(k.to_owned(), v.to_owned());
            }
            memo
        },
    ))
}

#[cfg(target_os = "windows")]
fn get_config_vars(_: &str) -> Result<HashMap<String, String>, String> {
    // sysconfig is missing all the flags on windows, so we can't actually
    // query the interpreter directly for its build flags.
    //
    // For the time being, this is the flags as defined in the python source's
    // PC\pyconfig.h. This won't work correctly if someone has built their
    // python with a modified pyconfig.h - sorry if that is you, you will have
    // to comment/uncomment the lines below.
    let mut map: HashMap<String, String> = HashMap::new();
    map.insert("Py_USING_UNICODE".to_owned(), "1".to_owned());
    map.insert("Py_UNICODE_WIDE".to_owned(), "0".to_owned());
    map.insert("WITH_THREAD".to_owned(), "1".to_owned());
    map.insert("Py_UNICODE_SIZE".to_owned(), "2".to_owned());

    // This is defined #ifdef _DEBUG. The visual studio build seems to produce
    // a specially named pythonXX_d.exe and pythonXX_d.dll when you build the
    // Debug configuration, which this script doesn't currently support anyway.
    // map.insert("Py_DEBUG", "1");

    // Uncomment these manually if your python was built with these and you want
    // the cfg flags to be set in rust.
    //
    // map.insert("Py_REF_DEBUG", "1");
    // map.insert("Py_TRACE_REFS", "1");
    // map.insert("COUNT_ALLOCS", 1");
    Ok(map)
}

fn is_value(key: &str) -> bool {
    SYSCONFIG_VALUES.iter().any(|x| *x == key)
}

fn cfg_line_for_var(key: &str, val: &str) -> Option<String> {
    if is_value(key) {
        // is a value; suffix the key name with the value
        Some(format!("cargo:rustc-cfg={}=\"{}_{}\"\n", CFG_KEY, key, val))
    } else if val != "0" {
        // is a flag that isn't zero
        Some(format!("cargo:rustc-cfg={}=\"{}\"", CFG_KEY, key))
    } else {
        // is a flag that is zero
        None
    }
}

fn is_not_none_or_zero(val: Option<&String>) -> bool {
    match val {
        Some(v) => v != "0",
        None => false,
    }
}

/// Run a python script using the specified interpreter binary.
fn run_python_script(interpreter: &str, script: &str) -> Result<String, String> {
    let mut cmd = Command::new(interpreter);
    cmd.arg("-c").arg(script);

    let out = cmd
        .output()
        .map_err(|e| format!("failed to run python interpreter `{:?}`: {}", cmd, e))?;

    if !out.status.success() {
        let stderr = String::from_utf8(out.stderr).unwrap();
        let mut msg = "python script failed with stderr:\n\n".to_string();
        msg.push_str(&stderr);
        return Err(msg);
    }

    Ok(String::from_utf8(out.stdout).unwrap())
}

#[cfg(not(target_os = "macos"))]
#[cfg(not(target_os = "windows"))]
#[allow(clippy::unnecessary_wraps)]
fn get_rustc_link_lib(
    _: &PythonVersion,
    ld_version: &str,
    enable_shared: bool,
) -> Result<String, String> {
    if enable_shared {
        Ok(format!("cargo:rustc-link-lib=python{}", ld_version))
    } else {
        Ok(format!("cargo:rustc-link-lib=static=python{}", ld_version))
    }
}

#[cfg(target_os = "macos")]
fn get_macos_linkmodel(expected_version: &PythonVersion) -> Result<String, String> {
    let script = "import sysconfig; print('framework' if sysconfig.get_config_var('PYTHONFRAMEWORK') else ('shared' if sysconfig.get_config_var('Py_ENABLE_SHARED') else 'static'));";
    let (_, interpreter_path, _) = find_interpreter_and_get_config(expected_version)?;
    let out = run_python_script(&interpreter_path, script).unwrap();
    Ok(out.trim_end().to_owned())
}

#[cfg(target_os = "macos")]
fn get_rustc_link_lib(
    expected_version: &PythonVersion,
    ld_version: &str,
    _: bool,
) -> Result<String, String> {
    // os x can be linked to a framework or static or dynamic, and
    // Py_ENABLE_SHARED is wrong; framework means shared library
    match get_macos_linkmodel(expected_version).unwrap().as_ref() {
        "static" => Ok(format!("cargo:rustc-link-lib=static=python{}", ld_version)),
        "shared" => Ok(format!("cargo:rustc-link-lib=python{}", ld_version)),
        "framework" => Ok(format!("cargo:rustc-link-lib=python{}", ld_version)),
        other => Err(format!("unknown linkmodel {}", other)),
    }
}

/// Parse string as interpreter version.
fn get_interpreter_version(line: &str) -> Result<PythonVersion, String> {
    let version_re = Regex::new(r"\((\d+), (\d+)\)").unwrap();
    match version_re.captures(&line) {
        Some(cap) => Ok(PythonVersion {
            major: cap.get(1).unwrap().as_str().parse().unwrap(),
            minor: Some(cap.get(2).unwrap().as_str().parse().unwrap()),
        }),
        None => Err(format!("Unexpected response to version query {}", line)),
    }
}

#[cfg(target_os = "windows")]
fn get_rustc_link_lib(version: &PythonVersion, _: &str, _: bool) -> Result<String, String> {
    // Py_ENABLE_SHARED doesn't seem to be present on windows.
    Ok(format!(
        "cargo:rustc-link-lib=pythonXY:python{}{}",
        version.major,
        match version.minor {
            Some(minor) => minor.to_string(),
            None => "".to_owned(),
        }
    ))
}

fn matching_version(expected_version: &PythonVersion, actual_version: &PythonVersion) -> bool {
    actual_version.major == expected_version.major
        && (expected_version.minor.is_none() || actual_version.minor == expected_version.minor)
}

/// Locate a suitable python interpreter and extract config from it.
/// If the environment variable `PYTHON_SYS_EXECUTABLE`, use the provided
/// path a Python executable, and raises an error if the version doesn't match.
/// Else tries to execute the interpreter as "python", "python{major version}",
/// "python{major version}.{minor version}" in order until one
/// is of the version we are expecting.
fn find_interpreter_and_get_config(
    expected_version: &PythonVersion,
) -> Result<(PythonVersion, String, Vec<String>), String> {
    if let Some(sys_executable) = env::var_os("PYTHON_SYS_EXECUTABLE") {
        let interpreter_path = sys_executable
            .to_str()
            .expect("Unable to get PYTHON_SYS_EXECUTABLE value");
        let (executable, interpreter_version, lines) =
            get_config_from_interpreter(interpreter_path)?;
        if matching_version(expected_version, &interpreter_version) {
            return Ok((interpreter_version, executable, lines));
        } else {
            return Err(format!(
                "Wrong python version in PYTHON_SYS_EXECUTABLE={}\n\
                 \texpected {} != found {}",
                executable, expected_version, interpreter_version
            ));
        }
    }

    let mut possible_names = vec![
        "python".to_string(),
        format!("python{}", expected_version.major),
    ];
    if let Some(minor) = expected_version.minor {
        possible_names.push(format!("python{}.{}", expected_version.major, minor));
    }

    for name in possible_names.iter() {
        if let Ok((executable, interpreter_version, lines)) = get_config_from_interpreter(name) {
            if matching_version(expected_version, &interpreter_version) {
                return Ok((interpreter_version, executable, lines));
            }
        }
    }
    Err(format!(
        "No python interpreter found of version {}",
        expected_version
    ))
}

/// Extract compilation vars from the specified interpreter.
fn get_config_from_interpreter(
    interpreter: &str,
) -> Result<(String, PythonVersion, Vec<String>), String> {
    let script = "import sys; import sysconfig; print(sys.executable); \
print(sys.version_info[0:2]); \
print(sysconfig.get_config_var('LIBDIR')); \
print(sysconfig.get_config_var('Py_ENABLE_SHARED')); \
print(sysconfig.get_config_var('LDVERSION') or '%s%s' % (sysconfig.get_config_var('py_version_short'), sysconfig.get_config_var('DEBUG_EXT') or '')); \
print(sys.exec_prefix);";
    let out = run_python_script(interpreter, script)?;
    let mut lines: Vec<String> = out
        .split(NEWLINE_SEQUENCE)
        .map(|line| line.to_owned())
        .collect();
    let executable = lines.remove(0);
    let interpreter_version = lines.remove(0);
    let interpreter_version = get_interpreter_version(&interpreter_version)?;
    Ok((executable, interpreter_version, lines))
}

/// Deduce configuration from the 'python' in the current PATH and print
/// cargo vars to stdout.
///
/// Note that if the python doesn't satisfy expected_version, this will error.
fn configure_from_path(expected_version: &PythonVersion) -> Result<String, String> {
    let (interpreter_version, interpreter_path, lines) =
        find_interpreter_and_get_config(expected_version)?;
    let libpath: &str = &lines[0];
    let enable_shared: &str = &lines[1];
    let ld_version: &str = &lines[2];
    let exec_prefix: &str = &lines[3];

    let is_extension_module = env::var_os("CARGO_FEATURE_EXTENSION_MODULE").is_some();
    let mut link_mode_default = env::var_os("CARGO_FEATURE_LINK_MODE_DEFAULT").is_some();
    let link_mode_unresolved_static =
        env::var_os("CARGO_FEATURE_LINK_MODE_UNRESOLVED_STATIC").is_some();

    if link_mode_default && link_mode_unresolved_static {
        return Err(
            "link-mode-default and link-mode-unresolved-static are mutually exclusive".to_owned(),
        );
    }

    if !link_mode_default && !link_mode_unresolved_static {
        link_mode_default = true;
    }

    if link_mode_default {
        if !is_extension_module || cfg!(target_os = "windows") {
            println!(
                "{}",
                get_rustc_link_lib(&interpreter_version, ld_version, enable_shared == "1").unwrap()
            );
            if libpath != "None" {
                println!("cargo:rustc-link-search=native={}", libpath);
            } else if cfg!(target_os = "windows") {
                println!("cargo:rustc-link-search=native={}\\libs", exec_prefix);
            }
        }
    } else if link_mode_unresolved_static && cfg!(target_os = "windows") {
        // static-nobundle requires a Nightly rustc up to at least
        // Rust 1.39 (https://github.com/rust-lang/rust/issues/37403).
        //
        // We need to use static linking on Windows to prevent symbol
        // name mangling. Otherwise Rust will prefix extern {} symbols
        // with __imp_. But if we used normal "static," we need a
        // pythonXY.lib at build time to package into the rlib.
        //
        // static-nobundle removes the build-time library requirement,
        // allowing a downstream consumer to provide the pythonXY library.
        println!("cargo:rustc-link-lib=static-nobundle=pythonXY");
    }

    if let PythonVersion {
        major: 3,
        minor: some_minor,
    } = interpreter_version
    {
        if env::var_os("CARGO_FEATURE_PEP_384").is_some() {
            println!("cargo:rustc-cfg=Py_LIMITED_API");
        }
        if let Some(minor) = some_minor {
            for i in 4..(minor + 1) {
                println!("cargo:rustc-cfg=Py_3_{}", i);
            }
        }
    }

    Ok(interpreter_path)
}

/// Determine the python version we're supposed to be building
/// from the features passed via the environment.
///
/// The environment variable can choose to omit a minor
/// version if the user doesn't care.
fn version_from_env() -> Result<PythonVersion, String> {
    let re = Regex::new(r"CARGO_FEATURE_PYTHON_(\d+)(_(\d+))?").unwrap();
    // sort env::vars so we get more explicit version specifiers first
    // so if the user passes e.g. the python-3 feature and the python-3-5
    // feature, python-3-5 takes priority.
    let mut vars = env::vars().collect::<Vec<_>>();
    vars.sort_by(|a, b| b.cmp(a));
    for (key, _) in vars {
        if let Some(cap) = re.captures(&key) {
            return Ok(PythonVersion {
                major: cap.get(1).unwrap().as_str().parse().unwrap(),
                minor: cap.get(3).map(|s| s.as_str().parse().unwrap()),
            });
        }
    }
    Err(
        "Python version feature was not found. At least one python version \
         feature must be enabled."
            .to_owned(),
    )
}

fn main() {
    // 1. Setup cfg variables so we can do conditional compilation in this
    // library based on the python interpeter's compilation flags. This is
    // necessary for e.g. matching the right unicode and threading interfaces.
    //
    // This locates the python interpreter based on the PATH, which should
    // work smoothly with an activated virtualenv.
    //
    // If you have troubles with your shell accepting '.' in a var name,
    // try using 'env' (sorry but this isn't our fault - it just has to
    // match the pkg-config package name, which is going to have a . in it).
    let version = version_from_env().unwrap();
    let python_interpreter_path = configure_from_path(&version).unwrap();
    let mut config_map = get_config_vars(&python_interpreter_path).unwrap();
    if is_not_none_or_zero(config_map.get("Py_DEBUG")) {
        config_map.insert("Py_TRACE_REFS".to_owned(), "1".to_owned()); // Py_DEBUG implies Py_TRACE_REFS.
    }
    if is_not_none_or_zero(config_map.get("Py_TRACE_REFS")) {
        config_map.insert("Py_REF_DEBUG".to_owned(), "1".to_owned()); // Py_TRACE_REFS implies Py_REF_DEBUG.
    }
    for (key, val) in &config_map {
        if let Some(line) = cfg_line_for_var(key, val) {
            println!("{}", line);
        }
    }

    // 2. Export python interpreter compilation flags as cargo variables that
    // will be visible to dependents. All flags will be available to dependent
    // build scripts in the environment variable DEP_PYTHON27_PYTHON_FLAGS as
    // comma separated list; each item in the list looks like
    //
    // {VAL,FLAG}_{flag_name}=val;
    //
    // FLAG indicates the variable is always 0 or 1
    // VAL indicates it can take on any value
    //
    // rust-cypthon/build.rs contains an example of how to unpack this data
    // into cfg flags that replicate the ones present in this library, so
    // you can use the same cfg syntax.
    let flags: String = config_map.iter().fold("".to_owned(), |memo, (key, val)| {
        if is_value(key) {
            memo + format!("VAL_{}={},", key, val).as_ref()
        } else if val != "0" {
            memo + format!("FLAG_{}={},", key, val).as_ref()
        } else {
            memo
        }
    });
    println!(
        "cargo:python_flags={}",
        if !flags.is_empty() {
            &flags[..flags.len() - 1]
        } else {
            ""
        }
    );

    // 3. Export Python interpreter path as a Cargo variable so dependent build
    // scripts can use invoke it.
    println!("cargo:python_interpreter={}", python_interpreter_path);
}
