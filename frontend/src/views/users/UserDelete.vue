<template>
  <div class="max-w-md mx-auto mt-10">
    <div class="bg-slate-800 border border-slate-700 rounded-xl p-6 shadow-xl">
      <div class="flex items-center space-x-3 text-red-500 mb-4">
        <svg class="h-8 w-8" fill="none" viewBox="0 0 24 24" stroke="currentColor">
          <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z" />
        </svg>
        <h2 class="text-xl font-bold text-slate-100">Delete User</h2>
      </div>

      <div v-if="loading" class="py-6 flex flex-col items-center justify-center">
        <div class="animate-spin rounded-full h-8 w-8 border-b-2 border-indigo-500"></div>
        <p class="text-slate-400 mt-2">Loading user details...</p>
      </div>

      <div v-else-if="error" class="bg-red-900/30 border border-red-500/50 text-red-200 px-4 py-3 rounded-lg mb-4 text-sm">
        {{ error }}
      </div>

      <div v-else-if="user">
        <p class="text-slate-300 mb-6">
          Are you sure you want to delete the user <strong class="text-white">{{ user.username }}</strong> ({{ user.email }})?
          This action <strong class="text-red-400">cannot be undone</strong>. All containers owned by this user may also be affected.
        </p>

        <div class="flex items-center justify-end space-x-3">
          <router-link
            to="/users"
            class="px-4 py-2 bg-slate-700 hover:bg-slate-600 text-slate-200 rounded-lg text-sm font-medium transition-colors"
          >
            Cancel
          </router-link>
          <button
            @click="handleDelete"
            :disabled="deleting"
            class="px-4 py-2 bg-red-600 hover:bg-red-700 disabled:opacity-50 text-white rounded-lg text-sm font-medium transition-colors flex items-center space-x-2"
          >
            <span v-if="deleting" class="animate-spin rounded-full h-4 w-4 border-b-2 border-white"></span>
            <span>Delete User</span>
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<script lang="ts">
import { defineComponent, ref, onMounted } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import api from '../../services/api';

interface User {
  id: number;
  username: string;
  email: string;
  role: string;
  is_active: boolean;
}

export default defineComponent({
  name: 'UserDelete',
  setup() {
    const route = useRoute();
    const router = useRouter();
    const userId = Number(route.params.id);
    
    const user = ref<User | null>(null);
    const loading = ref(true);
    const deleting = ref(false);
    const error = ref('');

    const fetchUser = async () => {
      try {
        loading.value = true;
        const res = await api.get(`/users/${userId}`);
        user.value = res.data;
      } catch (err: any) {
        error.value = err.response?.data?.detail || 'Failed to fetch user details';
      } finally {
        loading.value = false;
      }
    };

    const handleDelete = async () => {
      try {
        deleting.value = true;
        error.value = '';
        await api.delete(`/users/${userId}`);
        router.push('/users');
      } catch (err: any) {
        error.value = err.response?.data?.detail || 'Failed to delete user';
      } finally {
        deleting.value = false;
      }
    };

    onMounted(fetchUser);

    return {
      user,
      loading,
      deleting,
      error,
      handleDelete
    };
  }
});
</script>
