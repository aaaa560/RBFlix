import requests
from bs4 import BeautifulSoup
from selenium import webdriver
from selenium.webdriver.firefox.service import Service
from webdriver_manager.firefox import GeckoDriverManager
from selenium.webdriver.common.by import By
from selenium.webdriver.support.ui import WebDriverWait
from selenium.webdriver.support import expected_conditions as EC
import json
import re
from urllib.parse import urljoin


class VideoCrawler:
    def __init__(self):
        self.driver = None
        self.wait = None

    def start_driver(self):
        # Configura o Firefox para rodar em modo headless
        options = webdriver.FirefoxOptions()
        options.add_argument('--headless')  # Roda sem abrir a janela do navegador
        options.add_argument('--disable-gpu')
        options.add_argument('--no-sandbox')

        # Usa o GeckoDriverManager para baixar e gerenciar o driver do Firefox automaticamente
        self.driver = webdriver.Firefox(
            service=Service(GeckoDriverManager().install()),
            options=options
        )
        self.wait = WebDriverWait(self.driver, 10)

    def close_driver(self):
        if self.driver:
            self.driver.quit()

    def fetch_page(self, url):
        try:
            self.driver.get(url)
            return self.driver.page_source
        except Exception as e:
            print(f"Erro ao acessar {url}: {e}")
            return None

    def find_video_elements(self, html, base_url):
        soup = BeautifulSoup(html, 'html.parser')
        video_elements = []

        # Encontra todos os links que parecem ser de vídeos
        for link in soup.find_all('a', href=True):
            href = link['href']
            if not href.startswith(('http://', 'https://')):
                href = urljoin(base_url, href)

            # Verifica se o link parece ser de um vídeo
            if re.search(r'/watch\?|/video/|/v/', href, re.IGNORECASE):
                video_elements.append(link)

        return video_elements

    def extract_selectors(self, video_elements):
        selectors = {
            'video_selector': '',
            'title_selector': '',
            'thumbnail_selector': '',
            'duration_selector': '',
            'url_transform': 'None'
        }

        if not video_elements:
            return selectors

        # Pega o primeiro elemento de vídeo como exemplo
        example_element = video_elements[0]

        # Tenta encontrar o seletor CSS para o container do vídeo
        try:
            selectors['video_selector'] = self.get_css_selector(example_element)
        except:
            selectors['video_selector'] = 'a'  # Default: qualquer link

        # Tenta encontrar o seletor para o título
        title_element = example_element.find_parent().find('h1') or \
                        example_element.find_parent().find('h2') or \
                        example_element.find_parent().find('h3') or \
                        example_element.find_parent().find('span', class_=re.compile(r'title|name', re.I)) or \
                        example_element.find_parent().find('div', class_=re.compile(r'title|name', re.I))

        if title_element:
            selectors['title_selector'] = self.get_css_selector(title_element)
        else:
            selectors['title_selector'] = 'None'

        # Tenta encontrar o seletor para a thumbnail
        thumbnail_element = example_element.find('img') or \
                            example_element.find_parent().find('img')

        if thumbnail_element:
            selectors['thumbnail_selector'] = self.get_css_selector(thumbnail_element)
        else:
            selectors['thumbnail_selector'] = 'None'

        # Tenta encontrar o seletor para a duração
        duration_element = example_element.find_parent().find('span', class_=re.compile(r'duration|time', re.I)) or \
                           example_element.find_parent().find('div', class_=re.compile(r'duration|time', re.I))

        if duration_element:
            selectors['duration_selector'] = self.get_css_selector(duration_element)
        else:
            selectors['duration_selector'] = 'None'

        # Define a transformação de URL
        selectors['url_transform'] = 'Some(|href: &str| format!("{}{}", base_url, href))'

        return selectors

    def get_css_selector(self, element):
        components = []
        while element.parent and element.name != '[document]':
            sibling_index = self.get_sibling_index(element)
            if sibling_index is not None:
                components.append(f"{element.name}:nth-of-type({sibling_index + 1})")
            else:
                components.append(element.name)

            if element.get('id'):
                components[-1] += f"#{element['id']}"
                break
            elif element.get('class'):
                classes = ''.join(f".{c}" for c in element.get('class', []))
                components[-1] += classes
                break

            element = element.parent

        return ' > '.join(reversed(components))

    def get_sibling_index(self, element):
        parent = element.parent
        if not parent:
            return None

        siblings = parent.find_all(element.name, recursive=False)
        if not siblings:
            return None

        return siblings.index(element)

    def crawl_site(self, base_url, search_path, search_query):
        self.start_driver()

        try:
            search_url = f"{base_url}{search_path}{search_query}"
            print(f"Buscando em: {search_url}")
            html = self.fetch_page(search_url)

            if not html:
                return None

            video_elements = self.find_video_elements(html, base_url)
            if not video_elements:
                print("Nenhum elemento de vídeo encontrado.")
                return None

            selectors = self.extract_selectors(video_elements)

            # Salva os resultados em um arquivo JSON
            result = {
                'base_url': base_url,
                'search_path': search_path,
                'selectors': selectors,
                'example_video_elements': [
                    {
                        'title': elem.get_text(strip=True) if elem else 'N/A',
                        'href': elem.get('href') if elem else 'N/A',
                        'html': str(elem) if elem else 'N/A'
                    }
                    for elem in video_elements[:3]  # Pega os 3 primeiros como exemplo
                ]
            }

            filename = f"site_config_{base_url.replace('https://', '').replace('http://', '').replace('/', '_')}.json"
            with open(filename, 'w') as f:
                json.dump(result, f, indent=4)

            print(f"Configuração salva em {filename}")
            return result

        except Exception as e:
            print(f"Erro durante o crawling: {e}")
            return None
        finally:
            self.close_driver()


# Exemplo de uso
if __name__ == "__main__":
    crawler = VideoCrawler()

    # Exemplo para YouTube
    base_url = input('Insira a URL base do site: ')
    search_path = input('Insira o meio de pesquisa do site(EX.:/results?search_query=):')
    search_query = input('Insira a pesquisa: ')

    crawler.crawl_site(base_url, search_path, search_query)
