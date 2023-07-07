import os
import shutil

def copy_get(config, **kwargs):
    site_dir = config['site_dir']
    shutil.copy('scripts/install.sh', os.path.join(site_dir, 'get'))