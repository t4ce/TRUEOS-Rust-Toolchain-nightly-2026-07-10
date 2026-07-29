//! The AOT driver uses [`cranelift_object`] to write object files suitable for linking into a
//! standalone executable.

use std::convert::Infallible;
use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use cranelift_object::{ObjectBuilder, ObjectModule};
use rustc_ast::expand::allocator::AllocatorMethod;
use rustc_codegen_ssa::back::lto::ThinModule;
use rustc_codegen_ssa::back::write::{
    CodegenContext, FatLtoInput, ModuleConfig, SharedEmitter, TargetMachineFactoryFn, ThinLtoInput,
};
use rustc_codegen_ssa::traits::{ExtraBackendMethods, WriteBackendMethods};
use rustc_codegen_ssa::{CompiledModule, ModuleCodegen, ModuleKind};
use rustc_data_structures::profiling::SelfProfilerRef;
use rustc_errors::DiagCtxt;
use rustc_hir::attrs::Linkage as RLinkage;
use rustc_middle::dep_graph::WorkProduct;
use rustc_middle::middle::codegen_fn_attrs::CodegenFnAttrFlags;
use rustc_middle::mono::{MonoItem, MonoItemData, Visibility};
use rustc_session::Session;
use rustc_session::config::{OptLevel, OutputFilenames, OutputType};
use rustc_span::Symbol;

use crate::base::CodegenedFunction;
use crate::debuginfo::TypeDebugContext;
use crate::global_asm::{GlobalAsmConfig, GlobalAsmContext};
use crate::prelude::*;
use crate::unwind_module::UnwindModule;

pub(crate) struct AotModule {
    producer: String,
    global_asm_config: GlobalAsmConfig,
    module: UnwindModule<ObjectModule>,
    debug_context: Option<DebugContext>,
    codegened_functions: Vec<CodegenedFunction>,
    global_asm: String,
}

fn make_module(tcx: TyCtxt<'_>, cgu_name: &str) -> AotModule {
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: make module entry name={cgu_name}");
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: build isa begin name={cgu_name}");
    let isa = crate::build_isa(tcx.sess, false);
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: build isa complete name={cgu_name}");

    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: object builder begin name={cgu_name}");
    let mut builder = ObjectBuilder::new(
        isa,
        cgu_name.to_owned() + ".o",
        cranelift_module::default_libcall_names(),
    )
    .unwrap();
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: object builder complete name={cgu_name}");

    // Disable function sections by default on MSVC as it causes significant slowdowns with link.exe.
    // Maybe link.exe has exponential behavior when there are many sections with the same name? Also
    // explicitly disable it on MinGW as rustc already disables it by default on MinGW and as such
    // isn't tested. If rustc enables it in the future on MinGW, we can re-enable it too once it has
    // been on MinGW.
    let default_function_sections =
        tcx.sess.target.function_sections && !tcx.sess.target.is_like_windows;
    builder.per_function_section(
        tcx.sess.opts.unstable_opts.function_sections.unwrap_or(default_function_sections),
    );

    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: unwind module begin name={cgu_name}");
    let module = UnwindModule::new(ObjectModule::new(builder), true);
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: unwind module complete name={cgu_name}");

    let producer = crate::debuginfo::producer(tcx.sess);
    let global_asm_config = GlobalAsmConfig::new(tcx.sess);
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: debug context begin name={cgu_name}");
    let debug_context = DebugContext::new(tcx, module.isa(), false, cgu_name);
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: debug context complete name={cgu_name}");
    let codegened_functions = vec![];
    let global_asm = String::new();

    AotModule {
        producer,
        global_asm_config,
        module,
        debug_context,
        codegened_functions,
        global_asm,
    }
}

fn emit_module(
    output_filenames: &OutputFilenames,
    prof: &SelfProfilerRef,
    module: UnwindModule<ObjectModule>,
    debug: Option<DebugContext>,
    kind: ModuleKind,
    name: String,
    global_asm_object: Option<PathBuf>,
    producer_str: &str,
) -> Result<CompiledModule, String> {
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: emit module finish begin name={name}");
    let mut product = module.finish();
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: emit module finish complete name={name}");

    if let Some(mut debug) = debug {
        #[cfg(target_os = "trueos")]
        println!("TRUEOS rustc cranelift: debug emit begin name={name}");
        debug.emit(&mut product);
        #[cfg(target_os = "trueos")]
        println!("TRUEOS rustc cranelift: debug emit complete name={name}");
    }

    if product.object.format() == cranelift_object::object::BinaryFormat::Elf {
        let comment_section = product.object.add_section(
            Vec::new(),
            b".comment".to_vec(),
            cranelift_object::object::SectionKind::OtherString,
        );
        let mut producer = vec![0];
        producer.extend(producer_str.as_bytes());
        producer.push(0);
        product.object.set_section_data(comment_section, producer, 1);
    }

    let tmp_file = output_filenames.temp_path_for_cgu(OutputType::Object, &name);
    #[cfg(target_os = "trueos")]
    println!(
        "TRUEOS rustc cranelift: object file create begin name={name} path={}",
        tmp_file.display()
    );
    let file = match File::create(&tmp_file) {
        Ok(file) => file,
        Err(err) => return Err(format!("error creating object file: {}", err)),
    };
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: object file create complete name={name}");

    let mut file = BufWriter::new(file);
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: object write begin name={name}");
    if let Err(err) = product.object.write_stream(&mut file) {
        return Err(format!("error writing object file: {}", err));
    }
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: object write complete name={name}");
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: object flush begin name={name}");
    let file = match file.into_inner() {
        Ok(file) => file,
        Err(err) => return Err(format!("error writing object file: {}", err)),
    };
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: object flush complete name={name}");

    if prof.enabled() {
        prof.artifact_size(
            "object_file",
            tmp_file.file_name().unwrap().to_string_lossy(),
            file.metadata().unwrap().len(),
        );
    }

    Ok(CompiledModule {
        name,
        kind,
        object: Some(tmp_file),
        global_asm_object,
        dwarf_object: None,
        bytecode: None,
        assembly: None,
        llvm_ir: None,
        links_from_incr_cache: Vec::new(),
    })
}

fn codegen_cgu(tcx: TyCtxt<'_>, cgu_name: Symbol) -> AotModule {
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: codegen cgu entry name={cgu_name}");
    let _timer = tcx.prof.generic_activity_with_arg("codegen cgu", cgu_name.as_str());

    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: cgu query begin name={cgu_name}");
    let cgu = tcx.codegen_unit(cgu_name);
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: cgu query complete name={cgu_name}");
    let mono_items = cgu.items_in_deterministic_order(tcx);
    #[cfg(target_os = "trueos")]
    println!(
        "TRUEOS rustc cranelift: mono item order ready name={cgu_name} count={}",
        mono_items.len()
    );

    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: make module begin name={cgu_name}");
    let mut module = make_module(tcx, cgu_name.as_str());
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: make module complete name={cgu_name}");
    let mut type_dbg = TypeDebugContext::default();
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: predefine mono items begin name={cgu_name}");
    super::predefine_mono_items(tcx, &mut module.module, &mono_items);
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: predefine mono items complete name={cgu_name}");
    for (mono_index, (mono_item, item_data)) in mono_items.into_iter().enumerate() {
        match mono_item {
            MonoItem::Fn(instance) => {
                #[cfg(target_os = "trueos")]
                println!(
                    "TRUEOS rustc cranelift: mono fn begin name={cgu_name} index={mono_index}"
                );
                #[cfg(target_os = "trueos")]
                println!(
                    "TRUEOS rustc cranelift: mono fn attrs begin name={cgu_name} index={mono_index}"
                );
                let flags = tcx.codegen_instance_attrs(instance.def).flags;
                #[cfg(target_os = "trueos")]
                println!(
                    "TRUEOS rustc cranelift: mono fn attrs complete name={cgu_name} index={mono_index}"
                );
                #[cfg(target_os = "trueos")]
                println!(
                    "TRUEOS rustc cranelift: mono fn symbol begin name={cgu_name} index={mono_index}"
                );
                #[cfg(target_os = "trueos")]
                let mono_symbol = tcx.symbol_name(instance).name;
                #[cfg(target_os = "trueos")]
                println!(
                    "TRUEOS rustc cranelift: mono fn symbol complete name={cgu_name} index={mono_index} symbol={mono_symbol}"
                );
                if flags.contains(CodegenFnAttrFlags::NAKED) {
                    rustc_codegen_ssa::mir::naked_asm::codegen_naked_asm(
                        &mut GlobalAsmContext { tcx, global_asm: &mut module.global_asm },
                        instance,
                        MonoItemData {
                            linkage: RLinkage::External,
                            visibility: if item_data.linkage == RLinkage::Internal {
                                Visibility::Hidden
                            } else {
                                item_data.visibility
                            },
                            ..item_data
                        },
                    );
                    continue;
                }
                #[cfg(target_os = "trueos")]
                println!(
                    "TRUEOS rustc cranelift: mono fn lowering begin name={cgu_name} index={mono_index}"
                );
                #[cfg(target_os = "trueos")]
                println!(
                    "TRUEOS rustc cranelift: empty function begin name={cgu_name} index={mono_index}"
                );
                let empty_function = Function::new();
                #[cfg(target_os = "trueos")]
                println!(
                    "TRUEOS rustc cranelift: empty function complete name={cgu_name} index={mono_index}"
                );
                #[cfg(target_os = "trueos")]
                println!(
                    "TRUEOS rustc cranelift: codegen-fn call begin name={cgu_name} index={mono_index}"
                );
                let codegened_function = crate::base::codegen_fn(
                    tcx,
                    cgu.name(),
                    module.debug_context.as_mut(),
                    &mut type_dbg,
                    empty_function,
                    &mut module.module,
                    instance,
                );
                #[cfg(target_os = "trueos")]
                println!(
                    "TRUEOS rustc cranelift: codegen-fn call complete name={cgu_name} index={mono_index}"
                );
                #[cfg(target_os = "trueos")]
                println!(
                    "TRUEOS rustc cranelift: mono fn lowering complete name={cgu_name} index={mono_index}"
                );
                module.codegened_functions.push(codegened_function);
                #[cfg(target_os = "trueos")]
                println!(
                    "TRUEOS rustc cranelift: mono fn complete name={cgu_name} index={mono_index}"
                );
            }
            MonoItem::Static(def_id) => {
                #[cfg(target_os = "trueos")]
                println!(
                    "TRUEOS rustc cranelift: mono static begin name={cgu_name} index={mono_index}"
                );
                let data_id = crate::constant::codegen_static(tcx, &mut module.module, def_id);
                if let Some(debug_context) = module.debug_context.as_mut() {
                    debug_context.define_static(tcx, &mut type_dbg, def_id, data_id);
                }
                #[cfg(target_os = "trueos")]
                println!(
                    "TRUEOS rustc cranelift: mono static complete name={cgu_name} index={mono_index}"
                );
            }
            MonoItem::GlobalAsm(item_id) => {
                #[cfg(target_os = "trueos")]
                println!(
                    "TRUEOS rustc cranelift: mono global-asm begin name={cgu_name} index={mono_index}"
                );
                rustc_codegen_ssa::base::codegen_global_asm(
                    &mut GlobalAsmContext { tcx, global_asm: &mut module.global_asm },
                    item_id,
                );
                #[cfg(target_os = "trueos")]
                println!(
                    "TRUEOS rustc cranelift: mono global-asm complete name={cgu_name} index={mono_index}"
                );
            }
        }
    }
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: entry wrapper begin name={cgu_name}");
    crate::main_shim::maybe_create_entry_wrapper(tcx, &mut module.module, false, cgu.is_primary());
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: entry wrapper complete name={cgu_name}");

    module
}

fn compile_cgu(
    prof: &SelfProfilerRef,
    output_filenames: &OutputFilenames,
    should_write_ir: bool,
    mut aot_module: AotModule,
    cgu_name: String,
    kind: ModuleKind,
) -> Result<CompiledModule, String> {
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: compile functions begin name={cgu_name}");
    prof.generic_activity_with_arg("compile functions", &*cgu_name).run(|| {
        cranelift_codegen::timing::set_thread_profiler(Box::new(super::MeasuremeProfiler(
            prof.clone(),
        )));

        for (function_index, codegened_func) in
            aot_module.codegened_functions.into_iter().enumerate()
        {
            #[cfg(target_os = "trueos")]
            println!(
                "TRUEOS rustc cranelift: compile function begin name={cgu_name} index={function_index}"
            );
            // Do not carry Cranelift machine-code state across functions on
            // TrueOS. The compiler appliance has observed a deterministic
            // stall in `Module::define_function` after several successful
            // definitions when a cleared context is reused.
            let mut function_context = Context::new();
            crate::base::compile_fn(
                &prof,
                &output_filenames,
                should_write_ir,
                &mut function_context,
                &mut aot_module.module,
                aot_module.debug_context.as_mut(),
                &mut aot_module.global_asm,
                codegened_func,
            );
            #[cfg(target_os = "trueos")]
            println!(
                "TRUEOS rustc cranelift: compile function complete name={cgu_name} index={function_index}"
            );
        }
    });
    #[cfg(target_os = "trueos")]
    println!("TRUEOS rustc cranelift: compile functions complete name={cgu_name}");

    let global_asm_object_file =
        prof.generic_activity_with_arg("compile assembly", &*cgu_name).run(|| {
            if aot_module.global_asm.is_empty() {
                return Ok::<_, String>(None);
            }

            let global_asm_object_file = output_filenames.temp_path_ext_for_cgu("asm.o", &cgu_name);
            crate::global_asm::compile_global_asm(
                &aot_module.global_asm_config,
                aot_module.global_asm,
                &global_asm_object_file,
            )?;

            Ok(Some(global_asm_object_file))
        })?;

    prof.generic_activity_with_arg("write object file", &*cgu_name).run(|| {
        emit_module(
            output_filenames,
            prof,
            aot_module.module,
            aot_module.debug_context,
            kind,
            cgu_name.clone(),
            global_asm_object_file,
            &aot_module.producer,
        )
    })
}

#[derive(Copy, Clone)]
pub(crate) struct AotDriver;

impl ExtraBackendMethods for AotDriver {
    type Module = AotModule;

    fn codegen_allocator<'tcx>(
        &self,
        tcx: TyCtxt<'tcx>,
        module_name: &str,
        methods: &[AllocatorMethod],
    ) -> Self::Module {
        let mut allocator_module = make_module(tcx, module_name);
        crate::allocator::codegen(tcx, &mut allocator_module.module, methods);
        allocator_module
    }

    fn compile_codegen_unit(
        &self,
        tcx: TyCtxt<'_>,
        cgu_name: Symbol,
    ) -> (ModuleCodegen<Self::Module>, u64) {
        #[cfg(target_os = "trueos")]
        println!("TRUEOS rustc cranelift: compile-codegen-unit entry name={cgu_name}");
        let start_time = Instant::now();

        #[cfg(target_os = "trueos")]
        println!("TRUEOS rustc cranelift: dep node begin name={cgu_name}");
        let dep_node = tcx.codegen_unit(cgu_name).codegen_dep_node(tcx);
        #[cfg(target_os = "trueos")]
        println!("TRUEOS rustc cranelift: dep node complete name={cgu_name}");
        #[cfg(target_os = "trueos")]
        println!("TRUEOS rustc cranelift: dep task begin name={cgu_name}");
        let (module, _) = tcx.dep_graph.with_task(
            dep_node,
            tcx,
            || {
                let aot_module = codegen_cgu(tcx, cgu_name);
                ModuleCodegen::new_regular(cgu_name.as_str().to_owned(), aot_module)
            },
            Some(rustc_middle::dep_graph::hash_result),
        );
        #[cfg(target_os = "trueos")]
        println!("TRUEOS rustc cranelift: dep task complete name={cgu_name}");

        let time_to_codegen = start_time.elapsed();

        // We assume that the cost to run LLVM on a CGU is proportional to
        // the time we needed for codegenning it.
        let cost = time_to_codegen.as_nanos() as u64;

        (module, cost)
    }
}

impl WriteBackendMethods for AotDriver {
    type Module = AotModule;

    type TargetMachine = ();

    type ModuleBuffer = Infallible;

    type ThinData = Infallible;

    fn target_machine_factory(
        &self,
        _sess: &Session,
        _opt_level: OptLevel,
        _target_features: &[String],
    ) -> TargetMachineFactoryFn<Self> {
        Arc::new(|_, _| ())
    }

    fn optimize_and_codegen_fat_lto(
        _sess: &Session,
        _cgcx: &CodegenContext,
        _shared_emitter: &SharedEmitter,
        _tm_factory: TargetMachineFactoryFn<Self>,
        _exported_symbols_for_lto: &[String],
        _each_linked_rlib_for_lto: &[PathBuf],
        _modules: Vec<FatLtoInput<Self>>,
    ) -> CompiledModule {
        unreachable!()
    }

    fn run_thin_lto(
        _cgcx: &CodegenContext,
        _prof: &SelfProfilerRef,
        _dcx: rustc_errors::DiagCtxtHandle<'_>,
        _exported_symbols_for_lto: &[String],
        _each_linked_rlib_for_lto: &[PathBuf],
        _modules: Vec<ThinLtoInput<Self>>,
    ) -> (Vec<ThinModule<Self>>, Vec<WorkProduct>) {
        unreachable!()
    }

    fn optimize(
        _cgcx: &CodegenContext,
        _prof: &SelfProfilerRef,
        _shared_emitter: &SharedEmitter,
        _module: &mut ModuleCodegen<Self::Module>,
        _config: &ModuleConfig,
    ) {
    }

    fn optimize_and_codegen_thin(
        _cgcx: &CodegenContext,
        _prof: &SelfProfilerRef,
        _shared_emitter: &SharedEmitter,
        _tm_factory: TargetMachineFactoryFn<Self>,
        _thin: ThinModule<Self>,
    ) -> CompiledModule {
        unreachable!()
    }

    fn codegen(
        cgcx: &CodegenContext,
        prof: &SelfProfilerRef,
        shared_emitter: &SharedEmitter,
        module: ModuleCodegen<Self::Module>,
        config: &ModuleConfig,
    ) -> CompiledModule {
        compile_cgu(
            prof,
            &cgcx.output_filenames,
            config.emit_ir,
            module.module_llvm,
            module.name,
            module.kind,
        )
        .unwrap_or_else(|err| {
            let dcx = DiagCtxt::new(Box::new(shared_emitter.clone()));
            dcx.handle().fatal(err)
        })
    }

    fn serialize_module(_module: Self::Module, _is_thin: bool) -> Self::ModuleBuffer {
        unreachable!()
    }
}
