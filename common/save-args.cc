#include <inttypes.h>
#include <gcc-plugin.h>

#include <tree.h>
#include <tree-nested.h>
#include <tree-pass.h>
#include <cgraph.h>
#include <gimple.h>
#include <gimple-expr.h>
#include <gimple-iterator.h>
#include <plugin-version.h>
#include <stringpool.h>

// Assert that we're GPL compatible so we may be loaded into GCC
int plugin_is_GPL_compatible = 1;

static struct plugin_info saveargs_info = {
	.version = "0.1",
	.help = "pushes integer arguments onto the stack for later retrieval",
};

static const struct pass_data saveargs_data = {
		.type			= GIMPLE_PASS,
		.name			= "illumos-save-args",
		.optinfo_flags		= OPTGROUP_NONE,
		.tv_id			= TV_NONE,
		.properties_required	= PROP_cfg,
		.properties_provided	= 0,
		.properties_destroyed	= 0,
		.todo_flags_start	= 0,
		.todo_flags_finish	= TODO_verify_all
};

// XXX: I'm not sure why this isn't exposed
extern void error (const char *, ...) ATTRIBUTE_PRINTF_1 ATTRIBUTE_COLD;

class saveargs : public gimple_opt_pass {
public:
	saveargs(gcc::context *g) : gimple_opt_pass(saveargs_data, g) {};

	unsigned int
	execute(function *exec_fun) {
		tree fundecl = exec_fun->decl;
		basic_block on_entry = single_succ(ENTRY_BLOCK_PTR_FOR_FN(exec_fun));
		gimple_stmt_iterator gsi = gsi_start_bb(on_entry);
		gimple_seq seq = NULL;

		int nparams = 0;
		for (tree param = DECL_ARGUMENTS(fundecl); param;
		     param = DECL_CHAIN(param)) {
			// If we see a parameter not passed in the integer
			// registers, skip, we can't handle them properly.
			switch (DECL_MODE(param)) {
			case QImode: // char
			case HImode: // short
			case SImode: // int32
			case DImode: // int64
				break;
			default:
				error("save-args: unknown mode %d for param %d of %s\n",
				    DECL_MODE(param), nparams, function_name(exec_fun));
				// FALLTHROUGH
			case TImode: // "tetra"-integer, a 16byte int, used as a mode for small structs
			case OImode: // "octa"-integer, a 32byte int, used as the mode for small structs
			case XImode: // ...-integer, a 64byte int, used as the mode for small structs(?)
			case SFmode: // float
			case DFmode: // double
			case TFmode: // 128-bit long double
#if defined(XFmode)
			case XFmode: // 80-bit long-double
#endif
			case SCmode: // complex float
			case DCmode: // complex double
			case TCmode: // complex 128-bit long double
#if defined(XCmode)
			case XCmode: // complex 80-bit long double
#endif
			case BLKmode: // structure by value
				return (0);
			}

			nparams++;
		}

		if (nparams == 0)
			return (0);

		// Create an array of N pointers for each argument,
		// then assign each element in the array to its
		// argument.
		//
		// Note that this array is logically backwards, to
		// preserve the existing behaviour.
		//
		// XXX: But in v1 we have no reason to, which do we feel is
		// easiest?
		tree vol_ptr_type_node = build_pointer_type(build_type_variant(void_type_node, 0, 1));
		tree array_type = build_array_type_nelts(vol_ptr_type_node, nparams);
		tree name = get_identifier("__illumos_saved_args_v1__");
		location_t loc = DECL_SOURCE_LOCATION(fundecl);
		tree decl = build_decl(loc, VAR_DECL, name, array_type);
		SET_DECL_MODE(decl, BLKmode); // Force our structure into memory 

		DECL_CONTEXT(decl) = fundecl;
		DECL_ARTIFICIAL(decl) = true;
		TREE_USED(decl) = true;
		TREE_THIS_VOLATILE(decl) = true ;

	        add_local_decl(cfun, decl);

	        DECL_CHAIN(decl) = BLOCK_VARS(DECL_INITIAL(fundecl));
	        BLOCK_VARS(DECL_INITIAL(fundecl)) = decl;

		int i = nparams;
		for (tree param = DECL_ARGUMENTS(fundecl); param;
		     param = DECL_CHAIN(param), i--) {
			tree lhs = build4(ARRAY_REF, vol_ptr_type_node, decl,
			    build_int_cst(unsigned_type_node, i - 1),
			    NULL_TREE, NULL_TREE);

			gassign *gg = gimple_build_assign(lhs, param);
			gimple_set_location(gg, exec_fun->function_start_locus);
			gimple_seq_add_stmt(&seq, gg);
		}

		gsi_insert_seq_after(&gsi, seq, GSI_SAME_STMT);
		return (0);
	}
};

int
plugin_init(struct plugin_name_args *plugin_info,
    struct plugin_gcc_version *version)
{
	struct register_pass_info pass_info;
	extern gcc::context *g;

	if (!plugin_default_version_check(version, &gcc_version))
		return (1);

	pass_info.pass = new saveargs(g);
	pass_info.reference_pass_name = "cfg";
	pass_info.ref_pass_instance_number = 1;
	pass_info.pos_op = PASS_POS_INSERT_AFTER;

	register_callback(plugin_info->base_name, PLUGIN_INFO, NULL, &saveargs_info);
	register_callback(plugin_info->base_name, PLUGIN_PASS_MANAGER_SETUP,
	    NULL, &pass_info);

	return (0);
}
