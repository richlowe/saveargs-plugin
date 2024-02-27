#include <stdio.h>

#include <gcc-plugin.h>
#include <plugin-version.h>
#include <tree.h>
#include <tree-pass.h>
#include <tree-nested.h>
#include <gimple.h>
#include <gimple-expr.h>
#include <gimple-iterator.h>

extern "C" {
int plugin_is_GPL_compatible = 1;

int
plugin_init(struct plugin_name_args *plugin_info,
	    struct plugin_gcc_version *version);
}

static const struct pass_data saveargs_data = {
		.type			= GIMPLE_PASS,
		.name			= "illumos-save-args",
		.optinfo_flags		= OPTGROUP_NONE,
		.tv_id			= TV_NONE,
		.properties_required	= PROP_ssa | PROP_cfg,
		.properties_provided	= 0,
		.properties_destroyed	= 0,
		.todo_flags_start	= 0,
		.todo_flags_finish	= TODO_verify_all | TODO_update_ssa
};

class saveargs : public gimple_opt_pass {
public:
	saveargs(gcc::context *g) : gimple_opt_pass(saveargs_data, g) {
		printf("We're created\n");
	}

	bool gate(function *gate_fun) { return (true); }

	unsigned int
	execute(function *exec_fun) {
		printf("We're running for %s!\n", current_function_name());

		basic_block on_entry = single_succ(ENTRY_BLOCK_PTR_FOR_FN(cfun));
		gimple_stmt_iterator gsi = gsi_start_bb(on_entry);
		gimple_seq seq = NULL;

		int nparams = 0;
		for (tree param = DECL_ARGUMENTS(current_function_decl); param;
		     param = DECL_CHAIN(param)) {
			nparams++;
		}

		printf("%s has %d arguments\n", current_function_name(),
		    nparams);

		if (nparams == 0)
			return (0);

		// Create an array of N pointers for each argument,
		// then assign each element in the array to its
		// argument.
		//
		// Note that this array is logically backwards, to
		// preserve the existing behaviour.
		tree array_type = build_array_type_nelts(ptr_type_node, nparams);
		tree args_array = create_tmp_var(array_type, "__illumos_saved_args");
		TREE_THIS_VOLATILE(args_array) = 1;
		mark_addressable(args_array);

		int i = nparams;
		for (tree param = DECL_ARGUMENTS(current_function_decl); param;
		     param = DECL_CHAIN(param), i--) {
			tree lhs = build4(ARRAY_REF, ptr_type_node, args_array,
			    build_int_cst(unsigned_type_node, i - 1),
			    NULL_TREE, NULL_TREE);

			gassign *gg = gimple_build_assign(lhs, param);
			gimple_set_location(gg, cfun->function_start_locus);
			gimple_seq_add_stmt(&seq, gg);
		}

		gsi_insert_seq_before(&gsi, seq, GSI_SAME_STMT);
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
	pass_info.reference_pass_name = "ssa";
	pass_info.ref_pass_instance_number = 1;
	pass_info.pos_op = PASS_POS_INSERT_AFTER;

	register_callback(plugin_info->base_name, PLUGIN_PASS_MANAGER_SETUP,
	    NULL, &pass_info);

	return (0);
}
