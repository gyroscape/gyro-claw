from rembg import remove
from PIL import Image

def remove_background(input_path, output_path):
    print(f"Removing background for {input_path}...")
    input_img = Image.open(input_path)
    output_img = remove(input_img)
    output_img.save(output_path, "PNG")
    print(f"Saved transparent image to {output_path}")

remove_background("assets/logo.png", "assets/logo_transparent.png")
